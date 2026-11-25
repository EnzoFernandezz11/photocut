//! Hilo de trabajo en segundo plano para decodificación, detección, IA, previews y exportación.
//!
//! Mantiene `ScanCache::new(3)` acotado en RAM, coalesca solicitudes rápidas de
//! preview para evitar lag al arrastrar sliders y permite revisar el escaneo #1
//! mientras los restantes se indexan incrementalmente.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use eframe::egui;
use image::RgbImage;
use photocut_core::cache::ScanCache;
use photocut_core::detect::{DetectConfig, detect_photos};
use photocut_core::export::{SampleQuality, extract_photo, save_image};
use photocut_core::orient::OrientationModel;
use photocut_core::session::{ScanEntry, Session};
use photocut_core::{Adjustments, ExportFormat, OrientedRect, QuarterTurn};
use rayon::prelude::*;

const PREVIEW_MAX_DIM: u32 = 1400;
const BACKDROP_MAX_DIM: u32 = 1600;
const AI_CROP_MAX_DIM: u32 = 416;
const AI_MIN_CONFIDENCE: f32 = 0.70;
const SESSION_FILENAME: &str = ".photocut.json";

/// Comandos enviados desde la interfaz hacia el hilo de trabajo.
#[derive(Debug)]
pub enum WorkerCmd {
    OpenFolder {
        input_dir: PathBuf,
        output_dir: PathBuf,
        use_ai_orient: bool,
    },
    RenderPreview {
        seq: u64,
        scan_idx: usize,
        photo_id: usize,
        crop: OrientedRect,
        turn: QuarterTurn,
        adjustments: Adjustments,
        show_original: bool,
    },
    LoadScanBackdrop {
        scan_idx: usize,
    },
    ExportPhoto {
        scan_idx: usize,
        photo_id: usize,
        crop: OrientedRect,
        turn: QuarterTurn,
        adjustments: Adjustments,
        format: ExportFormat,
        output_path: PathBuf,
    },
    SaveSession {
        session: Session,
    },
    Shutdown,
}

/// Eventos emitidos por el hilo de trabajo hacia la interfaz.
#[derive(Debug)]
pub enum WorkerEvent {
    AiModelStatus {
        available: bool,
        detail: String,
    },
    FolderOpened {
        input_dir: PathBuf,
        output_dir: PathBuf,
        export_format: ExportFormat,
        scans: Vec<ScanEntry>,
        indexed_flags: Vec<bool>,
        total_scans: usize,
    },
    ScanIndexed {
        scan_idx: usize,
        entry: ScanEntry,
        scan_dims: (u32, u32),
        indexed_count: usize,
        total_scans: usize,
    },
    ScanningFinished {
        total_scans: usize,
    },
    PreviewReady {
        seq: u64,
        scan_idx: usize,
        photo_id: usize,
        image: RgbImage,
        full_dims: (u32, u32),
        show_original: bool,
    },
    BackdropReady {
        scan_idx: usize,
        image: RgbImage,
        orig_dims: (u32, u32),
    },
    ExportDone {
        scan_idx: usize,
        photo_id: usize,
        output_path: PathBuf,
        dims: (u32, u32),
        elapsed_ms: f64,
    },
    Error {
        context: String,
        message: String,
    },
}

/// Manejador del hilo de trabajo con cierre limpio garantizado en `Drop`.
pub struct WorkerHandle {
    cmd_tx: Sender<WorkerCmd>,
    evt_rx: Receiver<WorkerEvent>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Inicia el hilo de trabajo cargando el modelo ONNX opcional si existe.
    pub fn spawn(repaint_ctx: Option<egui::Context>) -> Self {
        Self::spawn_with_options(repaint_ctx, true)
    }

    /// Inicia el hilo de trabajo con opción de omitir carga ONNX (útil en tests).
    pub fn spawn_with_options(repaint_ctx: Option<egui::Context>, load_ai_model: bool) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (evt_tx, evt_rx) = mpsc::channel();

        let join = thread::Builder::new()
            .name("photocut-worker".to_string())
            .spawn(move || {
                let mut worker = WorkerState::new(evt_tx, repaint_ctx);
                if load_ai_model {
                    worker.load_default_ai_model();
                } else {
                    worker.emit(WorkerEvent::AiModelStatus {
                        available: false,
                        detail: "Desactivado".to_string(),
                    });
                }
                worker.run_loop(cmd_rx);
            })
            .expect("spawn photocut-worker thread");

        Self {
            cmd_tx,
            evt_rx,
            join: Some(join),
        }
    }

    pub fn send(&self, cmd: WorkerCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn try_recv(&self) -> Option<WorkerEvent> {
        self.evt_rx.try_recv().ok()
    }

    /// Detiene el hilo de trabajo y espera a que finalice sin colgarse.
    pub fn shutdown(&mut self) {
        let _ = self.cmd_tx.send(WorkerCmd::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct ActiveFolder {
    input_dir: PathBuf,
    output_dir: PathBuf,
    export_format: ExportFormat,
    use_ai_orient: bool,
    scan_paths: Vec<PathBuf>,
    scans: Vec<ScanEntry>,
    indexed_flags: Vec<bool>,
    indexed_count: usize,
    pending_queue: VecDeque<usize>,
}

impl ActiveFolder {
    fn session_path(&self) -> PathBuf {
        self.output_dir.join(SESSION_FILENAME)
    }

    fn save_indexed_session(&self) -> std::io::Result<()> {
        let indexed_scans: Vec<ScanEntry> = self
            .scans
            .iter()
            .zip(self.indexed_flags.iter())
            .filter_map(|(entry, &is_indexed)| is_indexed.then(|| entry.clone()))
            .collect();

        let session = Session {
            input_dir: self.input_dir.clone(),
            output_dir: self.output_dir.clone(),
            export_format: self.export_format,
            scans: indexed_scans,
        };
        session.save_atomic(&self.session_path())
    }
}

struct WorkerState {
    cache: ScanCache,
    ai_model: Option<OrientationModel>,
    folder: Option<ActiveFolder>,
    evt_tx: Sender<WorkerEvent>,
    repaint_ctx: Option<egui::Context>,
}

impl WorkerState {
    fn new(evt_tx: Sender<WorkerEvent>, repaint_ctx: Option<egui::Context>) -> Self {
        Self {
            cache: ScanCache::new(3),
            ai_model: None,
            folder: None,
            evt_tx,
            repaint_ctx,
        }
    }

    fn emit(&self, event: WorkerEvent) {
        let _ = self.evt_tx.send(event);
        if let Some(ctx) = &self.repaint_ctx {
            ctx.request_repaint();
        }
    }

    fn load_default_ai_model(&mut self) {
        match OrientationModel::find_default() {
            Some(path) => match OrientationModel::load(&path) {
                Ok(model) => {
                    self.ai_model = Some(model);
                    self.emit(WorkerEvent::AiModelStatus {
                        available: true,
                        detail: path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("ONNX")
                            .to_string(),
                    });
                }
                Err(err) => {
                    self.emit(WorkerEvent::AiModelStatus {
                        available: false,
                        detail: format!("Error al cargar modelo: {err}"),
                    });
                }
            },
            None => {
                self.emit(WorkerEvent::AiModelStatus {
                    available: false,
                    detail: "Modelo ONNX no encontrado (rotación manual)".to_string(),
                });
            }
        }
    }

    fn run_loop(&mut self, cmd_rx: Receiver<WorkerCmd>) {
        loop {
            let has_pending_scan = self
                .folder
                .as_ref()
                .is_some_and(|f| !f.pending_queue.is_empty());

            let mut batch = Vec::new();
            if has_pending_scan {
                loop {
                    match cmd_rx.try_recv() {
                        Ok(cmd) => batch.push(cmd),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
            } else {
                match cmd_rx.recv() {
                    Ok(cmd) => {
                        batch.push(cmd);
                        while let Ok(more) = cmd_rx.try_recv() {
                            batch.push(more);
                        }
                    }
                    Err(_) => return,
                }
            }

            if self.process_command_batch(batch) {
                return;
            }

            // Si no llegaron comandos urgentes, avanzar con el siguiente escaneo pendiente.
            self.index_next_pending_scan();
        }
    }

    /// Ejecuta un lote de comandos coalescando `RenderPreview`, `LoadScanBackdrop` y `SaveSession`.
    /// Devuelve `true` si se solicitó `Shutdown`.
    fn process_command_batch(&mut self, batch: Vec<WorkerCmd>) -> bool {
        if batch.is_empty() {
            return false;
        }

        let mut latest_preview: Option<WorkerCmd> = None;
        let mut latest_backdrop: Option<usize> = None;
        let mut latest_save: Option<Session> = None;
        let mut ordered: Vec<WorkerCmd> = Vec::with_capacity(batch.len());

        for cmd in batch {
            match cmd {
                WorkerCmd::Shutdown => return true,
                WorkerCmd::RenderPreview { .. } => latest_preview = Some(cmd),
                WorkerCmd::LoadScanBackdrop { scan_idx } => latest_backdrop = Some(scan_idx),
                WorkerCmd::SaveSession { session } => latest_save = Some(session),
                WorkerCmd::OpenFolder { .. } => {
                    // Al abrir nueva carpeta, descartar previews/backdrops/saves obsoletos anteriores.
                    latest_preview = None;
                    latest_backdrop = None;
                    latest_save = None;
                    ordered.clear();
                    ordered.push(cmd);
                }
                other => ordered.push(other),
            }
        }

        for cmd in ordered {
            match cmd {
                WorkerCmd::OpenFolder {
                    input_dir,
                    output_dir,
                    use_ai_orient,
                } => self.handle_open_folder(input_dir, output_dir, use_ai_orient),
                WorkerCmd::ExportPhoto {
                    scan_idx,
                    photo_id,
                    crop,
                    turn,
                    adjustments,
                    format,
                    output_path,
                } => self.handle_export_photo(
                    scan_idx,
                    photo_id,
                    crop,
                    turn,
                    adjustments,
                    format,
                    output_path,
                ),
                _ => {}
            }
        }

        if let Some(session) = latest_save {
            self.handle_save_session(session);
        }
        if let Some(scan_idx) = latest_backdrop {
            self.handle_load_backdrop(scan_idx);
        }
        if let Some(WorkerCmd::RenderPreview {
            seq,
            scan_idx,
            photo_id,
            crop,
            turn,
            adjustments,
            show_original,
        }) = latest_preview
        {
            self.handle_render_preview(
                seq,
                scan_idx,
                photo_id,
                crop,
                turn,
                adjustments,
                show_original,
            );
        }

        false
    }

    fn handle_open_folder(&mut self, input_dir: PathBuf, output_dir: PathBuf, use_ai_orient: bool) {
        self.cache.clear();

        let skip_dir = canonical_or_self(&output_dir);
        let mut scan_paths = Vec::new();
        collect_scans_recursive(&input_dir, &skip_dir, &mut scan_paths);
        scan_paths.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));

        let session_path = output_dir.join(SESSION_FILENAME);
        let existing = if session_path.is_file() {
            match Session::load(&session_path) {
                Ok(s) => Some(s),
                Err(err) => {
                    self.emit(WorkerEvent::Error {
                        context: "Sesión previa".to_string(),
                        message: format!(
                            "No se pudo leer {}: {err}",
                            session_path.display()
                        ),
                    });
                    None
                }
            }
        } else {
            None
        };

        let export_format = existing
            .as_ref()
            .map(|s| s.export_format)
            .unwrap_or_default();

        let mut existing_map: HashMap<String, ScanEntry> = existing
            .map(|s| {
                s.scans
                    .into_iter()
                    .map(|e| (normalize_rel(&e.rel_path), e))
                    .collect()
            })
            .unwrap_or_default();

        let mut scans = Vec::with_capacity(scan_paths.len());
        let mut indexed_flags = Vec::with_capacity(scan_paths.len());
        let mut pending_queue = VecDeque::new();
        let mut indexed_count = 0usize;

        for (idx, path) in scan_paths.iter().enumerate() {
            let rel_path = path
                .strip_prefix(&input_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");

            if let Some(mut saved_entry) = existing_map.remove(&rel_path) {
                saved_entry.rel_path = rel_path;
                scans.push(saved_entry);
                indexed_flags.push(true);
                indexed_count += 1;
            } else {
                scans.push(ScanEntry {
                    rel_path,
                    photos: Vec::new(),
                });
                indexed_flags.push(false);
                pending_queue.push_back(idx);
            }
        }

        let total_scans = scan_paths.len();
        let folder = ActiveFolder {
            input_dir: input_dir.clone(),
            output_dir: output_dir.clone(),
            export_format,
            use_ai_orient,
            scan_paths,
            scans: scans.clone(),
            indexed_flags: indexed_flags.clone(),
            indexed_count,
            pending_queue,
        };

        let finished_immediately = folder.pending_queue.is_empty();
        self.folder = Some(folder);

        self.emit(WorkerEvent::FolderOpened {
            input_dir,
            output_dir,
            export_format,
            scans,
            indexed_flags,
            total_scans,
        });

        if finished_immediately {
            self.emit(WorkerEvent::ScanningFinished { total_scans });
        }
    }

    fn index_next_pending_scan(&mut self) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        let Some(scan_idx) = folder.pending_queue.pop_front() else {
            return;
        };

        let scan_path = folder.scan_paths[scan_idx].clone();
        let rel_path = folder.scans[scan_idx].rel_path.clone();
        let use_ai = folder.use_ai_orient;

        let scan = match self.cache.load_or_get(&scan_path) {
            Ok(img) => img,
            Err(err) => {
                if let Some(f) = self.folder.as_mut() {
                    f.indexed_flags[scan_idx] = true;
                    f.indexed_count += 1;
                }
                self.emit(WorkerEvent::Error {
                    context: rel_path,
                    message: format!("Error al abrir imagen: {err}"),
                });
                self.check_scanning_finished();
                return;
            }
        };

        let scan_dims = scan.dimensions();
        let mut photos = detect_photos(&scan, DetectConfig::default());

        if use_ai
            && let Some(model) = &self.ai_model
        {
            for photo in &mut photos {
                let sample = extract_photo(
                    &scan,
                    photo.crop,
                    QuarterTurn::Deg0,
                    neutral_adjustments(),
                    SampleQuality::Bilinear,
                    Some(AI_CROP_MAX_DIM),
                );
                if let Ok(pred) = model.predict(&sample, AI_MIN_CONFIDENCE) {
                    photo.turn = pred.turn;
                }
            }
        }

        let entry = ScanEntry { rel_path, photos };

        let (indexed_count, total_scans, save_err) = {
            let f = self.folder.as_mut().expect("active folder");
            f.scans[scan_idx] = entry.clone();
            f.indexed_flags[scan_idx] = true;
            f.indexed_count += 1;
            let err = f.save_indexed_session().err();
            (f.indexed_count, f.scans.len(), err)
        };

        if let Some(err) = save_err {
            self.emit(WorkerEvent::Error {
                context: "Guardar sesión".to_string(),
                message: err.to_string(),
            });
        }

        self.emit(WorkerEvent::ScanIndexed {
            scan_idx,
            entry,
            scan_dims,
            indexed_count,
            total_scans,
        });

        self.check_scanning_finished();
    }

    fn check_scanning_finished(&self) {
        if let Some(f) = &self.folder
            && f.pending_queue.is_empty()
        {
            self.emit(WorkerEvent::ScanningFinished {
                total_scans: f.scans.len(),
            });
        }
    }

    fn handle_render_preview(
        &mut self,
        seq: u64,
        scan_idx: usize,
        photo_id: usize,
        crop: OrientedRect,
        turn: QuarterTurn,
        adjustments: Adjustments,
        show_original: bool,
    ) {
        let Some(scan_path) = self
            .folder
            .as_ref()
            .and_then(|f| f.scan_paths.get(scan_idx))
            .cloned()
        else {
            return;
        };

        let scan = match self.cache.load_or_get(&scan_path) {
            Ok(img) => img,
            Err(err) => {
                self.emit(WorkerEvent::Error {
                    context: "Vista previa".to_string(),
                    message: format!("No se pudo leer {}: {err}", scan_path.display()),
                });
                return;
            }
        };

        let effective_adj = if show_original {
            neutral_adjustments()
        } else {
            adjustments
        };

        let image = extract_photo(
            &scan,
            crop,
            turn,
            effective_adj,
            SampleQuality::Bilinear,
            Some(PREVIEW_MAX_DIM),
        );

        let full_dims = full_output_dims(crop, turn);
        self.emit(WorkerEvent::PreviewReady {
            seq,
            scan_idx,
            photo_id,
            image,
            full_dims,
            show_original,
        });
    }

    fn handle_load_backdrop(&mut self, scan_idx: usize) {
        let Some(scan_path) = self
            .folder
            .as_ref()
            .and_then(|f| f.scan_paths.get(scan_idx))
            .cloned()
        else {
            return;
        };

        let scan = match self.cache.load_or_get(&scan_path) {
            Ok(img) => img,
            Err(err) => {
                self.emit(WorkerEvent::Error {
                    context: "Re-recortar".to_string(),
                    message: format!("No se pudo cargar el escaneo completo: {err}"),
                });
                return;
            }
        };

        let orig_dims = scan.dimensions();
        let image = downsample_box_parallel(&scan, BACKDROP_MAX_DIM);
        self.emit(WorkerEvent::BackdropReady {
            scan_idx,
            image,
            orig_dims,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_export_photo(
        &mut self,
        scan_idx: usize,
        photo_id: usize,
        crop: OrientedRect,
        turn: QuarterTurn,
        adjustments: Adjustments,
        format: ExportFormat,
        output_path: PathBuf,
    ) {
        let Some(scan_path) = self
            .folder
            .as_ref()
            .and_then(|f| f.scan_paths.get(scan_idx))
            .cloned()
        else {
            return;
        };

        let t0 = Instant::now();
        let scan = match self.cache.load_or_get(&scan_path) {
            Ok(img) => img,
            Err(err) => {
                self.emit(WorkerEvent::Error {
                    context: "Exportar".to_string(),
                    message: format!("No se pudo abrir escaneo original: {err}"),
                });
                return;
            }
        };

        let full = extract_photo(
            &scan,
            crop,
            turn,
            adjustments,
            SampleQuality::Bicubic,
            None,
        );
        let dims = full.dimensions();

        if let Err(err) = save_image(&full, &output_path, format) {
            self.emit(WorkerEvent::Error {
                context: "Exportar".to_string(),
                message: format!("No se pudo guardar {}: {err}", output_path.display()),
            });
            return;
        }

        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.emit(WorkerEvent::ExportDone {
            scan_idx,
            photo_id,
            output_path,
            dims,
            elapsed_ms,
        });
    }

    fn handle_save_session(&mut self, incoming: Session) {
        if let Some(folder) = self.folder.as_mut() {
            folder.output_dir = incoming.output_dir.clone();
            folder.export_format = incoming.export_format;

            let by_rel: HashMap<&str, &ScanEntry> = incoming
                .scans
                .iter()
                .map(|s| (s.rel_path.as_str(), s))
                .collect();

            for (i, entry) in folder.scans.iter_mut().enumerate() {
                if folder.indexed_flags[i]
                    && let Some(updated) = by_rel.get(entry.rel_path.as_str())
                {
                    *entry = (*updated).clone();
                }
            }

            if let Err(err) = folder.save_indexed_session() {
                self.emit(WorkerEvent::Error {
                    context: "Guardar sesión".to_string(),
                    message: err.to_string(),
                });
            }
        } else {
            let path = incoming.output_dir.join(SESSION_FILENAME);
            if let Err(err) = incoming.save_atomic(&path) {
                self.emit(WorkerEvent::Error {
                    context: "Guardar sesión".to_string(),
                    message: err.to_string(),
                });
            }
        }
    }
}

pub fn full_output_dims(crop: OrientedRect, turn: QuarterTurn) -> (u32, u32) {
    let w = crop.width.round().max(1.0) as u32;
    let h = crop.height.round().max(1.0) as u32;
    match turn {
        QuarterTurn::Deg0 | QuarterTurn::Deg180 => (w, h),
        QuarterTurn::Deg90 | QuarterTurn::Deg270 => (h, w),
    }
}

fn neutral_adjustments() -> Adjustments {
    Adjustments {
        auto_color_strength: 0.0,
        brightness: 0.0,
        contrast: 0.0,
        saturation: 0.0,
        temperature: 0.0,
        tint: 0.0,
    }
}

fn normalize_rel(s: &str) -> String {
    s.replace('\\', "/")
}

fn canonical_or_self(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn collect_scans_recursive(dir: &Path, skip_dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with('.')
        {
            continue;
        }
        if path.is_dir() {
            if canonical_or_self(&path) == skip_dir {
                continue;
            }
            collect_scans_recursive(&path, skip_dir, out);
        } else if is_supported_image(&path) {
            out.push(path);
        }
    }
}

fn is_supported_image(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff"
    )
}

/// Ordenamiento natural para que `img2.jpg` aparezca antes que `img10.jpg`.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut ia = a.chars().peekable();
    let mut ib = b.chars().peekable();

    loop {
        match (ia.peek(), ib.peek()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(&c) = ia.peek() {
                    if c.is_ascii_digit() {
                        na.push(c);
                        ia.next();
                    } else {
                        break;
                    }
                }
                let mut nb = String::new();
                while let Some(&c) = ib.peek() {
                    if c.is_ascii_digit() {
                        nb.push(c);
                        ib.next();
                    } else {
                        break;
                    }
                }
                let va = na.trim_start_matches('0');
                let vb = nb.trim_start_matches('0');
                let ord = va
                    .len()
                    .cmp(&vb.len())
                    .then_with(|| va.cmp(vb))
                    .then_with(|| na.len().cmp(&nb.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(_), Some(_)) => {
                let ca = ia.next().unwrap().to_ascii_lowercase();
                let cb = ib.next().unwrap().to_ascii_lowercase();
                if ca != cb {
                    return ca.cmp(&cb);
                }
            }
        }
    }
}

/// Downsample paralelo rápido por promedio de caja para cargar el fondo de Re-recortar (~12 ms en 36MP).
fn downsample_box_parallel(src: &RgbImage, max_dim: u32) -> RgbImage {
    let (sw, sh) = src.dimensions();
    let longest = sw.max(sh);
    if longest <= max_dim {
        return src.clone();
    }
    let scale = max_dim as f32 / longest as f32;
    let dw = ((sw as f32 * scale).round() as u32).max(1);
    let dh = ((sh as f32 * scale).round() as u32).max(1);

    let sx_ratio = sw as f32 / dw as f32;
    let sy_ratio = sh as f32 / dh as f32;

    let src_raw = src.as_raw();
    let sw_u = sw as usize;
    let dw_u = dw as usize;
    let mut dst_raw = vec![0u8; dw_u * dh as usize * 3];

    dst_raw
        .par_chunks_mut(dw_u * 3)
        .enumerate()
        .for_each(|(dy, row)| {
            let y0 = (dy as f32 * sy_ratio) as usize;
            let y1 = (((dy + 1) as f32 * sy_ratio).ceil() as usize)
                .min(sh as usize)
                .max(y0 + 1);
            for dx in 0..dw_u {
                let x0 = (dx as f32 * sx_ratio) as usize;
                let x1 = (((dx + 1) as f32 * sx_ratio).ceil() as usize)
                    .min(sw_u)
                    .max(x0 + 1);
                let mut r = 0u32;
                let mut g = 0u32;
                let mut b = 0u32;
                let mut count = 0u32;
                for sy in y0..y1 {
                    let base = sy * sw_u * 3;
                    for sx in x0..x1 {
                        let i = base + sx * 3;
                        r += src_raw[i] as u32;
                        g += src_raw[i + 1] as u32;
                        b += src_raw[i + 2] as u32;
                        count += 1;
                    }
                }
                let out = dx * 3;
                row[out] = (r / count) as u8;
                row[out + 1] = (g / count) as u8;
                row[out + 2] = (b / count) as u8;
            }
        });

    RgbImage::from_raw(dw, dh, dst_raw).expect("valid downsampled backdrop buffer")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;
    use photocut_core::ReviewStatus;
    use std::time::Duration;

    fn make_synthetic_scan(w: u32, h: u32) -> RgbImage {
        let mut img = RgbImage::from_pixel(w, h, Rgb([252, 252, 252]));
        for y in 40..(h - 40) {
            for x in 50..(w - 50) {
                img.put_pixel(x, y, Rgb([110, 80, 140]));
            }
        }
        img
    }

    fn recv_until<F, T>(worker: &WorkerHandle, timeout: Duration, mut pred: F) -> T
    where
        F: FnMut(WorkerEvent) -> Option<T>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(ev) = worker.try_recv()
                && let Some(found) = pred(ev)
            {
                return found;
            }
            assert!(Instant::now() < deadline, "timed out waiting for worker event");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn worker_spawns_renders_preview_exports_and_shuts_down_cleanly() {
        let base = std::env::temp_dir().join(format!(
            "photocut_worker_basic_{}_{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let in_dir = base.join("in");
        let out_dir = base.join("out");
        fs::create_dir_all(&in_dir).unwrap();
        make_synthetic_scan(320, 420)
            .save(in_dir.join("img001.png"))
            .unwrap();

        let mut worker = WorkerHandle::spawn_with_options(None, false);
        worker.send(WorkerCmd::OpenFolder {
            input_dir: in_dir.clone(),
            output_dir: out_dir.clone(),
            use_ai_orient: false,
        });

        let indexed = recv_until(&worker, Duration::from_secs(5), |ev| match ev {
            WorkerEvent::ScanIndexed { entry, .. } => Some(entry),
            _ => None,
        });
        assert_eq!(indexed.photos.len(), 1);
        let photo = &indexed.photos[0];

        // Preview
        worker.send(WorkerCmd::RenderPreview {
            seq: 1,
            scan_idx: 0,
            photo_id: photo.id,
            crop: photo.crop,
            turn: QuarterTurn::Deg90,
            adjustments: Adjustments::default(),
            show_original: false,
        });
        let (seq, preview_dims) = recv_until(&worker, Duration::from_secs(5), |ev| match ev {
            WorkerEvent::PreviewReady { seq, image, .. } => Some((seq, image.dimensions())),
            _ => None,
        });
        assert_eq!(seq, 1);
        assert!(preview_dims.0 > 0 && preview_dims.1 > 0);

        // Backdrop
        worker.send(WorkerCmd::LoadScanBackdrop { scan_idx: 0 });
        let orig_dims = recv_until(&worker, Duration::from_secs(5), |ev| match ev {
            WorkerEvent::BackdropReady { orig_dims, .. } => Some(orig_dims),
            _ => None,
        });
        assert_eq!(orig_dims, (320, 420));

        // Full-res bicubic export
        let export_path = out_dir.join("img001_foto01.png");
        worker.send(WorkerCmd::ExportPhoto {
            scan_idx: 0,
            photo_id: photo.id,
            crop: photo.crop,
            turn: QuarterTurn::Deg0,
            adjustments: Adjustments::default(),
            format: ExportFormat::Png,
            output_path: export_path.clone(),
        });
        let exported = recv_until(&worker, Duration::from_secs(5), |ev| match ev {
            WorkerEvent::ExportDone {
                output_path, dims, ..
            } => Some((output_path, dims)),
            _ => None,
        });
        assert_eq!(exported.0, export_path);
        assert!(export_path.is_file());
        assert!(exported.1.0 >= 200 && exported.1.1 >= 300);

        // Clean shutdown never hangs
        worker.shutdown();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn worker_coalesces_rapid_preview_requests() {
        let base = std::env::temp_dir().join(format!(
            "photocut_worker_coalesce_{}",
            std::process::id()
        ));
        let in_dir = base.join("in");
        let out_dir = base.join("out");
        fs::create_dir_all(&in_dir).unwrap();
        make_synthetic_scan(400, 500)
            .save(in_dir.join("img001.png"))
            .unwrap();

        let mut worker = WorkerHandle::spawn_with_options(None, false);
        worker.send(WorkerCmd::OpenFolder {
            input_dir: in_dir.clone(),
            output_dir: out_dir.clone(),
            use_ai_orient: false,
        });
        let photo = recv_until(&worker, Duration::from_secs(5), |ev| match ev {
            WorkerEvent::ScanIndexed { entry, .. } => Some(entry.photos[0].clone()),
            _ => None,
        });

        // Enviar una ráfaga de 25 solicitudes de preview simulando arrastre rápido de slider.
        let total_burst = 25u64;
        for seq in 1..=total_burst {
            worker.send(WorkerCmd::RenderPreview {
                seq,
                scan_idx: 0,
                photo_id: photo.id,
                crop: photo.crop,
                turn: QuarterTurn::Deg0,
                adjustments: Adjustments {
                    brightness: (seq as f32 / total_burst as f32) - 0.5,
                    ..Default::default()
                },
                show_original: false,
            });
        }

        let mut received_seqs = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(WorkerEvent::PreviewReady { seq, .. }) = worker.try_recv() {
                received_seqs.push(seq);
                if seq == total_burst {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(received_seqs.last().copied(), Some(total_burst));
        assert!(
            received_seqs.len() < total_burst as usize,
            "expected coalescing to drop stale previews, got all {:?}",
            received_seqs
        );

        worker.shutdown();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn open_folder_preserves_existing_approved_session_entries() {
        let base = std::env::temp_dir().join(format!(
            "photocut_worker_resume_{}",
            std::process::id()
        ));
        let in_dir = base.join("in");
        let out_dir = base.join("out");
        fs::create_dir_all(&in_dir).unwrap();
        make_synthetic_scan(260, 340)
            .save(in_dir.join("img001.png"))
            .unwrap();
        make_synthetic_scan(260, 340)
            .save(in_dir.join("img002.png"))
            .unwrap();

        // Guardar una sesión previa donde img001 ya fue aprobada con un recorte custom.
        let custom_crop = OrientedRect {
            cx: 130.0,
            cy: 170.0,
            width: 150.0,
            height: 220.0,
            angle_deg: 2.5,
        };
        let prior = Session {
            input_dir: in_dir.clone(),
            output_dir: out_dir.clone(),
            export_format: ExportFormat::Tiff,
            scans: vec![ScanEntry {
                rel_path: "img001.png".to_string(),
                photos: vec![photocut_core::DetectedPhoto {
                    id: 0,
                    crop: custom_crop,
                    turn: QuarterTurn::Deg180,
                    adjustments: Adjustments::default(),
                    status: ReviewStatus::Approved,
                }],
            }],
        };
        prior.save_atomic(&out_dir.join(SESSION_FILENAME)).unwrap();

        let mut worker = WorkerHandle::spawn_with_options(None, false);
        worker.send(WorkerCmd::OpenFolder {
            input_dir: in_dir.clone(),
            output_dir: out_dir.clone(),
            use_ai_orient: false,
        });

        let (fmt, scans, flags) = recv_until(&worker, Duration::from_secs(5), |ev| match ev {
            WorkerEvent::FolderOpened {
                export_format,
                scans,
                indexed_flags,
                ..
            } => Some((export_format, scans, indexed_flags)),
            _ => None,
        });

        assert_eq!(fmt, ExportFormat::Tiff);
        assert_eq!(scans.len(), 2);
        assert_eq!(flags, vec![true, false]);
        assert_eq!(scans[0].photos[0].status, ReviewStatus::Approved);
        assert_eq!(scans[0].photos[0].crop, custom_crop);
        assert_eq!(scans[0].photos[0].turn, QuarterTurn::Deg180);

        // El segundo escaneo debe indexarse en segundo plano.
        let idx2 = recv_until(&worker, Duration::from_secs(5), |ev| match ev {
            WorkerEvent::ScanIndexed {
                scan_idx, entry, ..
            } => Some((scan_idx, entry)),
            _ => None,
        });
        assert_eq!(idx2.0, 1);
        assert_eq!(idx2.1.photos.len(), 1);

        worker.shutdown();
        let _ = fs::remove_dir_all(&base);
    }
}
