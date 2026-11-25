//! Aplicación principal de escritorio (`eframe::App`) con gestor multi-trabajo y visor darkroom.
//!
//! Higiene de texturas estricta: mantiene como máximo 2 `TextureHandle` en GPU
//! (`preview_tex` y `backdrop_tex`), sobreescribiéndolos in-place sin fugas de VRAM.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Align, Color32, ComboBox, CornerRadius, Frame, Key, Layout, Margin, Pos2, Rect,
    RichText, ScrollArea, Sense, Slider, SliderClamping, StrokeKind, TextEdit, TextureHandle,
    TextureOptions, Vec2,
};
use image::RgbImage;
use photocut_core::jobs::{
    JobRecord, JobRegistry, JobStatus, compute_idle_status, default_output_dir,
    discover_job_subfolders, suggest_job_name,
};
use photocut_core::session::{ScanEntry, Session};
use photocut_core::{
    Adjustments, DetectedPhoto, ExportFormat, OrientedRect, QuarterTurn, ReviewStatus,
};

use crate::crop_editor::{CropEditorState, MIN_CROP_SIZE, show_crop_editor};
use crate::icons::{
    ButtonStyle, Icon, danger_icon_button, draw_icon, icon_button, icon_button_sized,
    primary_icon_button, small_icon_button, styled_icon_button, subtle_icon_button,
};
use crate::theme;
use crate::worker::{QueuedJob, WorkerCmd, WorkerEvent, WorkerHandle, full_output_dims};

/// Argumentos opcionales de línea de comandos para abrir una carpeta directamente.
#[derive(Debug, Default, Clone)]
pub struct CliArgs {
    pub input_dir: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    MainMenu,
    Review { job_id: Option<u64> },
}

struct NewJobDraft {
    name: String,
    input_dir: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    output_is_auto: bool,
    export_format: ExportFormat,
    jpeg_quality: u8,
    use_ai_orient: bool,
}

impl Default for NewJobDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            input_dir: None,
            output_dir: None,
            output_is_auto: true,
            export_format: ExportFormat::Png,
            jpeg_quality: 96,
            use_ai_orient: true,
        }
    }
}

struct ActiveQueueUi {
    job_id: u64,
    job_name: String,
    job_index: usize,
    total_jobs: usize,
    indexed_scans: usize,
    total_scans: usize,
    total_photos: usize,
    current_scan_name: String,
    cancelling: bool,
}

struct PreviewMeta {
    scan_idx: usize,
    photo_id: usize,
    tex_dims: (u32, u32),
    full_dims: (u32, u32),
    show_original: bool,
}

struct BackdropMeta {
    scan_idx: usize,
    orig_dims: (u32, u32),
}

enum StatusKind {
    Info,
    Success,
    Error,
}

struct StatusBanner {
    kind: StatusKind,
    text: String,
}

pub struct PhotoCutApp {
    worker: WorkerHandle,

    // Pantalla actual y registro persistente de trabajos
    screen: Screen,
    registry_path: PathBuf,
    registry: JobRegistry,
    show_new_job_strip: bool,
    new_job_draft: NewJobDraft,
    queue_state: Option<ActiveQueueUi>,

    // Estado de la carpeta / sesión abierta en pantalla de Revisión
    input_dir: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    output_is_auto: bool,
    export_format: ExportFormat,
    last_jpeg_quality: u8,
    scans: Vec<ScanEntry>,
    indexed_flags: Vec<bool>,
    total_scans: usize,
    scanning_active: bool,

    // Orientación IA
    ai_model_loading: bool,
    ai_orient_available: bool,
    ai_orient_enabled: bool,
    ai_status_detail: String,

    // Recortes originales detectados (en memoria) y dimensiones de escaneo
    original_crops: HashMap<(usize, usize), OrientedRect>,
    scan_dims: HashMap<usize, (u32, u32)>,

    // Selección y modo de vista
    selected_scan: usize,
    selected_photo_pos: usize,
    recrop_mode: bool,
    crop_editor: CropEditorState,
    show_original_held: bool,
    compare_mouse_held: bool,

    // Zoom y paneo en el visor normal
    zoom: f32,
    pan: Vec2,

    // Texturas GPU (estrictamente acotadas a 2 slots: sin fugas de memoria/VRAM)
    preview_tex: Option<TextureHandle>,
    preview_meta: Option<PreviewMeta>,
    backdrop_tex: Option<TextureHandle>,
    backdrop_meta: Option<BackdropMeta>,
    backdrop_requested_scan: Option<usize>,

    // Seguimiento de solicitudes en vuelo
    next_preview_seq: u64,
    exports_in_flight: usize,
    pending_new_crop_recenter: Option<(usize, usize)>,
    status_banner: Option<StatusBanner>,
}

impl PhotoCutApp {
    pub fn new(ctx: egui::Context, cli: CliArgs) -> Self {
        let worker = WorkerHandle::spawn(Some(ctx));
        let registry_path = JobRegistry::default_path();
        let mut registry = JobRegistry::load_or_default(&registry_path);
        registry.refresh_all();

        let mut app = Self {
            worker,
            screen: Screen::MainMenu,
            registry_path,
            registry,
            show_new_job_strip: false,
            new_job_draft: NewJobDraft::default(),
            queue_state: None,
            input_dir: None,
            output_dir: None,
            output_is_auto: true,
            export_format: ExportFormat::default(),
            last_jpeg_quality: 96,
            scans: Vec::new(),
            indexed_flags: Vec::new(),
            total_scans: 0,
            scanning_active: false,
            ai_model_loading: true,
            ai_orient_available: false,
            ai_orient_enabled: true,
            ai_status_detail: "Cargando modelo de orientación automática...".to_string(),
            original_crops: HashMap::new(),
            scan_dims: HashMap::new(),
            selected_scan: 0,
            selected_photo_pos: 0,
            recrop_mode: false,
            crop_editor: CropEditorState::default(),
            show_original_held: false,
            compare_mouse_held: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            preview_tex: None,
            preview_meta: None,
            backdrop_tex: None,
            backdrop_meta: None,
            backdrop_requested_scan: None,
            next_preview_seq: 0,
            exports_in_flight: 0,
            pending_new_crop_recenter: None,
            status_banner: None,
        };

        if let Some(input_dir) = cli.input_dir {
            let (output_dir, is_auto) = match cli.output_dir {
                Some(out) => (out, false),
                None => (default_output_dir(&input_dir), true),
            };
            let job_id = app.registry.add_or_update_job(
                suggest_job_name(&input_dir),
                input_dir.clone(),
                output_dir.clone(),
                app.export_format,
                app.ai_orient_enabled,
            );
            app.save_registry();
            app.output_is_auto = is_auto;
            app.screen = Screen::Review {
                job_id: Some(job_id),
            };
            app.open_folder(input_dir, output_dir, app.ai_orient_enabled);
        }

        app
    }

    fn save_registry(&mut self) {
        if let Err(err) = self.registry.save_atomic(&self.registry_path) {
            self.status_banner = Some(StatusBanner {
                kind: StatusKind::Error,
                text: format!("No se pudo guardar la lista de trabajos: {err}"),
            });
        }
    }

    fn sync_active_job_to_registry(&mut self) {
        let Screen::Review {
            job_id: Some(job_id),
        } = self.screen
        else {
            return;
        };

        let indexed_scans = self.indexed_flags.iter().filter(|&&f| f).count();
        let (approved, skipped, total_photos) = self.photo_status_counts();
        let total_scans = self.total_scans;
        let fmt = self.export_format;
        let use_ai = self.ai_orient_enabled;
        let in_dir = self.input_dir.clone();
        let out_dir = self.output_dir.clone();

        if let Some(job) = self.registry.find_mut(job_id) {
            if let Some(p) = in_dir {
                job.input_dir = p;
            }
            if let Some(p) = out_dir {
                job.output_dir = p;
            }
            job.export_format = fmt;
            job.use_ai_orient = use_ai;
            job.total_scans = total_scans;
            job.indexed_scans = indexed_scans;
            job.total_photos = total_photos;
            job.approved_photos = approved;
            job.skipped_photos = skipped;
            job.status = compute_idle_status(
                total_scans,
                indexed_scans,
                total_photos,
                approved,
                skipped,
            );
        }
        self.save_registry();
    }

    fn return_to_main_menu(&mut self) {
        self.save_session();
        self.sync_active_job_to_registry();
        self.screen = Screen::MainMenu;
        self.recrop_mode = false;
        self.crop_editor.clear_drag();
        self.show_original_held = false;
        self.preview_tex = None;
        self.preview_meta = None;
        self.backdrop_tex = None;
        self.backdrop_meta = None;
        self.backdrop_requested_scan = None;
        self.registry.refresh_all();
        self.save_registry();
    }

    fn open_job_for_review(&mut self, job_id: u64) {
        let Some(job) = self.registry.find(job_id).cloned() else {
            return;
        };
        self.export_format = job.export_format;
        if let ExportFormat::Jpeg { quality } = job.export_format {
            self.last_jpeg_quality = quality;
        }
        if self.ai_orient_available {
            self.ai_orient_enabled = job.use_ai_orient;
        }
        self.output_is_auto = job.output_dir == default_output_dir(&job.input_dir);
        self.screen = Screen::Review {
            job_id: Some(job.id),
        };
        self.open_folder(job.input_dir, job.output_dir, job.use_ai_orient);
    }

    fn start_queue_for_jobs(&mut self, job_ids: Vec<u64>) {
        if self.queue_state.is_some() {
            return;
        }
        let mut queued = Vec::new();
        for id in job_ids {
            if let Some(job) = self.registry.find_mut(id) {
                job.status = JobStatus::Processing;
                queued.push(QueuedJob {
                    job_id: job.id,
                    input_dir: job.input_dir.clone(),
                    output_dir: job.output_dir.clone(),
                    export_format: job.export_format,
                    use_ai_orient: job.use_ai_orient && self.ai_orient_available,
                });
            }
        }
        if queued.is_empty() {
            return;
        }

        let first = &queued[0];
        let first_name = self
            .registry
            .find(first.job_id)
            .map(|j| j.name.clone())
            .unwrap_or_else(|| "Trabajo".to_string());
        let first_total_scans = self
            .registry
            .find(first.job_id)
            .map(|j| j.total_scans)
            .unwrap_or(0);

        self.queue_state = Some(ActiveQueueUi {
            job_id: first.job_id,
            job_name: first_name,
            job_index: 0,
            total_jobs: queued.len(),
            indexed_scans: 0,
            total_scans: first_total_scans,
            total_photos: 0,
            current_scan_name: "Iniciando...".to_string(),
            cancelling: false,
        });
        self.status_banner = None;
        self.save_registry();
        self.worker.send(WorkerCmd::StartJobQueue { jobs: queued });
    }

    fn cancel_job_queue(&mut self) {
        if let Some(q) = self.queue_state.as_mut() {
            q.cancelling = true;
        }
        self.worker.send(WorkerCmd::CancelJobQueue);
    }

    fn import_subfolders_dialog(&mut self) {
        let Some(parent) = rfd::FileDialog::new()
            .set_title("Elegir carpeta principal con subcarpetas de álbumes")
            .pick_folder()
        else {
            return;
        };

        let subfolders = discover_job_subfolders(&parent);
        if subfolders.is_empty() {
            self.status_banner = Some(StatusBanner {
                kind: StatusKind::Error,
                text: format!(
                    "No se encontraron subcarpetas con imágenes dentro de {}.",
                    parent.display()
                ),
            });
            return;
        }

        let count = subfolders.len();
        for sub in subfolders {
            let name = suggest_job_name(&sub);
            let out_dir = default_output_dir(&sub);
            self.registry.add_or_update_job(
                name,
                sub,
                out_dir,
                self.export_format,
                self.ai_orient_available && self.ai_orient_enabled,
            );
        }
        self.save_registry();
        self.status_banner = Some(StatusBanner {
            kind: StatusKind::Success,
            text: format!(
                "Se importaron {count} trabajos desde {}. Podés procesarlos todos juntos con \"Procesar todos\".",
                suggest_job_name(&parent)
            ),
        });
    }

    fn open_folder(&mut self, input_dir: PathBuf, output_dir: PathBuf, use_ai_orient: bool) {
        self.input_dir = Some(input_dir.clone());
        self.output_dir = Some(output_dir.clone());
        self.scans.clear();
        self.indexed_flags.clear();
        self.total_scans = 0;
        self.scanning_active = true;
        self.original_crops.clear();
        self.scan_dims.clear();
        self.selected_scan = 0;
        self.selected_photo_pos = 0;
        self.recrop_mode = false;
        self.crop_editor.clear_drag();
        self.show_original_held = false;
        self.reset_view();
        self.preview_tex = None;
        self.preview_meta = None;
        self.backdrop_tex = None;
        self.backdrop_meta = None;
        self.backdrop_requested_scan = None;
        self.pending_new_crop_recenter = None;
        self.status_banner = Some(StatusBanner {
            kind: StatusKind::Info,
            text: format!("Abriendo carpeta {}...", input_dir.display()),
        });

        self.worker.send(WorkerCmd::OpenFolder {
            input_dir,
            output_dir,
            use_ai_orient: use_ai_orient && self.ai_orient_available,
        });
    }

    fn pick_input_folder_in_review(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Seleccionar carpeta de escaneos");
        if let Some(dir) = &self.input_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(in_dir) = dialog.pick_folder() {
            let out_dir = if self.output_is_auto || self.output_dir.is_none() {
                self.output_is_auto = true;
                default_output_dir(&in_dir)
            } else {
                self.output_dir.clone().unwrap()
            };
            let job_id = self.registry.add_or_update_job(
                suggest_job_name(&in_dir),
                in_dir.clone(),
                out_dir.clone(),
                self.export_format,
                self.ai_orient_enabled,
            );
            self.save_registry();
            self.screen = Screen::Review {
                job_id: Some(job_id),
            };
            self.open_folder(in_dir, out_dir, self.ai_orient_enabled);
        }
    }

    fn pick_output_folder_in_review(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Seleccionar carpeta de salida");
        if let Some(dir) = self.output_dir.as_ref().or(self.input_dir.as_ref()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(out_dir) = dialog.pick_folder() {
            self.output_is_auto = false;
            self.output_dir = Some(out_dir.clone());
            if let Some(in_dir) = self.input_dir.clone() {
                if let Screen::Review {
                    job_id: Some(job_id),
                } = self.screen
                    && let Some(job) = self.registry.find_mut(job_id)
                {
                    job.output_dir = out_dir.clone();
                }
                self.save_registry();
                self.open_folder(in_dir, out_dir, self.ai_orient_enabled);
            }
        }
    }

    fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }

    fn current_photo(&self) -> Option<&DetectedPhoto> {
        self.scans
            .get(self.selected_scan)
            .and_then(|s| s.photos.get(self.selected_photo_pos))
    }

    fn current_photo_mut(&mut self) -> Option<&mut DetectedPhoto> {
        self.scans
            .get_mut(self.selected_scan)
            .and_then(|s| s.photos.get_mut(self.selected_photo_pos))
    }

    fn request_current_preview(&mut self) {
        let Some(photo) = self.current_photo().cloned() else {
            return;
        };
        self.next_preview_seq += 1;
        let seq = self.next_preview_seq;
        self.worker.send(WorkerCmd::RenderPreview {
            seq,
            scan_idx: self.selected_scan,
            photo_id: photo.id,
            crop: photo.crop,
            turn: photo.turn,
            adjustments: photo.adjustments,
            show_original: self.show_original_held,
        });
    }

    fn request_current_backdrop(&mut self) {
        if self.selected_scan >= self.scans.len() {
            return;
        }
        if self
            .backdrop_meta
            .as_ref()
            .is_some_and(|m| m.scan_idx == self.selected_scan)
            || self.backdrop_requested_scan == Some(self.selected_scan)
        {
            return;
        }
        self.backdrop_requested_scan = Some(self.selected_scan);
        self.worker.send(WorkerCmd::LoadScanBackdrop {
            scan_idx: self.selected_scan,
        });
    }

    fn save_session(&self) {
        let (Some(input_dir), Some(output_dir)) = (self.input_dir.clone(), self.output_dir.clone())
        else {
            return;
        };
        let session = Session {
            input_dir,
            output_dir,
            export_format: self.export_format,
            scans: self.scans.clone(),
        };
        self.worker.send(WorkerCmd::SaveSession { session });
    }

    fn select_photo(&mut self, scan_idx: usize, photo_pos: usize) {
        let Some(scan) = self.scans.get(scan_idx) else {
            return;
        };
        if photo_pos >= scan.photos.len() {
            return;
        }
        let changed = self.selected_scan != scan_idx || self.selected_photo_pos != photo_pos;
        self.selected_scan = scan_idx;
        self.selected_photo_pos = photo_pos;
        if changed {
            self.reset_view();
            self.crop_editor.reset_view();
        }
        if self.recrop_mode {
            self.request_current_backdrop();
        } else {
            self.request_current_preview();
        }
    }

    fn flat_photo_positions(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (si, scan) in self.scans.iter().enumerate() {
            for pi in 0..scan.photos.len() {
                out.push((si, pi));
            }
        }
        out
    }

    fn select_prev(&mut self) {
        let list = self.flat_photo_positions();
        if list.is_empty() {
            return;
        }
        let curr = (self.selected_scan, self.selected_photo_pos);
        if let Some(pos) = list.iter().position(|&p| p == curr) {
            if pos > 0 {
                let (s, p) = list[pos - 1];
                self.select_photo(s, p);
            }
        } else {
            let (s, p) = list[0];
            self.select_photo(s, p);
        }
    }

    fn select_next(&mut self) {
        let list = self.flat_photo_positions();
        if list.is_empty() {
            return;
        }
        let curr = (self.selected_scan, self.selected_photo_pos);
        if let Some(pos) = list.iter().position(|&p| p == curr) {
            if pos + 1 < list.len() {
                let (s, p) = list[pos + 1];
                self.select_photo(s, p);
            }
        } else {
            let (s, p) = list[0];
            self.select_photo(s, p);
        }
    }

    fn advance_after_review(&mut self) {
        let list = self.flat_photo_positions();
        if list.is_empty() {
            return;
        }
        let curr = (self.selected_scan, self.selected_photo_pos);
        if let Some(pos) = list.iter().position(|&p| p == curr) {
            if pos + 1 < list.len() {
                let (s, p) = list[pos + 1];
                self.select_photo(s, p);
                return;
            }
            if let Some(&(s, p)) = list
                .iter()
                .find(|&&(si, pi)| self.scans[si].photos[pi].status == ReviewStatus::Pending)
            {
                self.select_photo(s, p);
            }
        }
    }

    fn toggle_recrop_mode(&mut self) {
        if self.current_photo().is_none() {
            return;
        }
        self.recrop_mode = !self.recrop_mode;
        self.crop_editor.reset_view();
        self.show_original_held = false;
        if self.recrop_mode {
            self.request_current_backdrop();
        } else {
            self.save_session();
            self.request_current_preview();
        }
    }

    fn rotate_quarter(&mut self, cw: bool) {
        if let Some(photo) = self.current_photo_mut() {
            photo.turn = if cw {
                photo.turn.rotate_cw()
            } else {
                photo.turn.rotate_ccw()
            };
            self.save_session();
            if !self.recrop_mode {
                self.request_current_preview();
            }
        }
    }

    fn approve_and_export_current(&mut self) {
        if self.recrop_mode {
            self.recrop_mode = false;
            self.crop_editor.clear_drag();
        }
        let Some(output_dir) = self.output_dir.clone() else {
            return;
        };
        let Some(scan) = self.scans.get_mut(self.selected_scan) else {
            return;
        };
        let rel_path = scan.rel_path.clone();
        let Some(photo) = scan.photos.get_mut(self.selected_photo_pos) else {
            return;
        };
        photo.status = ReviewStatus::Approved;
        let snapshot = photo.clone();

        let output_path =
            build_export_path(&output_dir, &rel_path, snapshot.id, self.export_format);
        self.exports_in_flight += 1;
        self.worker.send(WorkerCmd::ExportPhoto {
            scan_idx: self.selected_scan,
            photo_id: snapshot.id,
            crop: snapshot.crop,
            turn: snapshot.turn,
            adjustments: snapshot.adjustments,
            format: self.export_format,
            output_path,
        });
        self.save_session();
        self.sync_active_job_to_registry();
        self.advance_after_review();
    }

    fn skip_current(&mut self) {
        if self.recrop_mode {
            self.recrop_mode = false;
            self.crop_editor.clear_drag();
        }
        if let Some(photo) = self.current_photo_mut() {
            photo.status = ReviewStatus::Skipped;
            self.save_session();
            self.sync_active_job_to_registry();
            self.advance_after_review();
        }
    }

    fn add_crop_to_current_scan(&mut self) {
        if self.scans.is_empty() {
            return;
        }
        let scan_idx = self.selected_scan.min(self.scans.len() - 1);
        let known_dims = self.scan_dims.get(&scan_idx).copied();
        let (sw, sh) = known_dims.unwrap_or((3000, 4000));

        let scan = &mut self.scans[scan_idx];
        let next_id = scan.photos.iter().map(|p| p.id + 1).max().unwrap_or(0);
        let crop = OrientedRect {
            cx: sw as f32 * 0.5,
            cy: sh as f32 * 0.5,
            width: (sw as f32 * 0.45).max(MIN_CROP_SIZE * 2.0),
            height: (sh as f32 * 0.45).max(MIN_CROP_SIZE * 2.0),
            angle_deg: 0.0,
        };
        scan.photos.push(DetectedPhoto {
            id: next_id,
            crop,
            turn: QuarterTurn::Deg0,
            adjustments: Adjustments::default(),
            status: ReviewStatus::Pending,
        });
        let new_pos = scan.photos.len() - 1;
        self.original_crops.insert((scan_idx, next_id), crop);
        self.pending_new_crop_recenter = if known_dims.is_none() {
            Some((scan_idx, next_id))
        } else {
            None
        };

        self.selected_scan = scan_idx;
        self.selected_photo_pos = new_pos;
        self.recrop_mode = true;
        self.crop_editor.reset_view();
        self.save_session();
        self.sync_active_job_to_registry();
        self.request_current_backdrop();
    }

    fn poll_worker_events(&mut self, ctx: &egui::Context) {
        while let Some(ev) = self.worker.try_recv() {
            match ev {
                WorkerEvent::AiModelStatus { available, detail } => {
                    self.ai_model_loading = false;
                    self.ai_orient_available = available;
                    if !available {
                        self.ai_orient_enabled = false;
                        self.new_job_draft.use_ai_orient = false;
                    }
                    self.ai_status_detail = detail;
                }
                WorkerEvent::FolderOpened {
                    input_dir,
                    output_dir,
                    export_format,
                    scans,
                    indexed_flags,
                    total_scans,
                } => {
                    self.input_dir = Some(input_dir);
                    self.output_dir = Some(output_dir);
                    self.export_format = export_format;
                    if let ExportFormat::Jpeg { quality } = export_format {
                        self.last_jpeg_quality = quality;
                    }
                    self.scans = scans;
                    self.indexed_flags = indexed_flags;
                    self.total_scans = total_scans;
                    self.scanning_active = self.indexed_flags.iter().any(|&f| !f);

                    self.original_crops.clear();
                    for (si, scan) in self.scans.iter().enumerate() {
                        for p in &scan.photos {
                            self.original_crops.insert((si, p.id), p.crop);
                        }
                    }

                    self.sync_active_job_to_registry();

                    if self.total_scans == 0 {
                        self.status_banner = Some(StatusBanner {
                            kind: StatusKind::Error,
                            text: "No se encontraron imágenes (JPG/PNG/TIFF) en la carpeta seleccionada."
                                .to_string(),
                        });
                    } else {
                        self.status_banner = None;
                        if let Some((si, pi)) = self.find_initial_selection() {
                            self.selected_scan = si;
                            self.selected_photo_pos = pi;
                            self.request_current_preview();
                        }
                    }
                }
                WorkerEvent::ScanIndexed {
                    scan_idx,
                    entry,
                    scan_dims,
                    indexed_count,
                    total_scans,
                } => {
                    self.scan_dims.insert(scan_idx, scan_dims);
                    let had_selection = self.current_photo().is_some();
                    if let Some(slot) = self.scans.get_mut(scan_idx) {
                        for p in &entry.photos {
                            self.original_crops.insert((scan_idx, p.id), p.crop);
                        }
                        *slot = entry;
                    }
                    if let Some(flag) = self.indexed_flags.get_mut(scan_idx) {
                        *flag = true;
                    }
                    self.total_scans = total_scans;
                    self.scanning_active = indexed_count < total_scans;
                    self.sync_active_job_to_registry();

                    if !had_selection && let Some((si, pi)) = self.find_initial_selection() {
                        self.selected_scan = si;
                        self.selected_photo_pos = pi;
                        self.request_current_preview();
                    }
                }
                WorkerEvent::ScanningFinished { total_scans } => {
                    self.total_scans = total_scans;
                    self.scanning_active = false;
                    self.sync_active_job_to_registry();
                }
                WorkerEvent::JobQueueProgress {
                    job_id,
                    job_index,
                    total_jobs,
                    indexed_scans,
                    total_scans,
                    total_photos,
                    current_scan_name,
                } => {
                    let job_name = self
                        .registry
                        .find(job_id)
                        .map(|j| j.name.clone())
                        .unwrap_or_else(|| format!("Trabajo #{job_id}"));

                    if let Some(job) = self.registry.find_mut(job_id) {
                        job.status = JobStatus::Processing;
                        job.total_scans = total_scans;
                        job.indexed_scans = indexed_scans;
                        job.total_photos = total_photos;
                    }

                    let cancelling = self.queue_state.as_ref().is_some_and(|q| q.cancelling);
                    self.queue_state = Some(ActiveQueueUi {
                        job_id,
                        job_name,
                        job_index,
                        total_jobs,
                        indexed_scans,
                        total_scans,
                        total_photos,
                        current_scan_name,
                        cancelling,
                    });
                }
                WorkerEvent::JobQueueJobFinished {
                    job_id,
                    total_scans,
                    indexed_scans,
                    total_photos,
                    approved_photos,
                    skipped_photos,
                    error,
                } => {
                    if let Some(job) = self.registry.find_mut(job_id) {
                        job.total_scans = total_scans;
                        job.indexed_scans = indexed_scans;
                        job.total_photos = total_photos;
                        job.approved_photos = approved_photos;
                        job.skipped_photos = skipped_photos;
                        job.status = match error {
                            Some(err) => JobStatus::Error(err),
                            None => compute_idle_status(
                                total_scans,
                                indexed_scans,
                                total_photos,
                                approved_photos,
                                skipped_photos,
                            ),
                        };
                    }
                    self.save_registry();
                }
                WorkerEvent::JobQueueFinished { cancelled } => {
                    self.queue_state = None;
                    for job in &mut self.registry.jobs {
                        if matches!(job.status, JobStatus::Processing) {
                            job.status = JobStatus::Pending;
                        }
                    }
                    self.registry.refresh_all();
                    self.save_registry();
                    self.status_banner = Some(StatusBanner {
                        kind: if cancelled {
                            StatusKind::Info
                        } else {
                            StatusKind::Success
                        },
                        text: if cancelled {
                            "Cola pausada. Todo lo analizado hasta ahora quedó guardado automáticamente."
                                .to_string()
                        } else {
                            "Cola completada. Ya podés revisar tus álbumes con calma.".to_string()
                        },
                    });
                }
                WorkerEvent::PreviewReady {
                    seq,
                    scan_idx,
                    photo_id,
                    image,
                    full_dims,
                    show_original,
                } => {
                    let is_current = self
                        .current_photo()
                        .is_some_and(|p| self.selected_scan == scan_idx && p.id == photo_id);
                    if !is_current || seq < self.next_preview_seq {
                        continue;
                    }
                    let tex_dims = image.dimensions();
                    upload_rgb_texture(ctx, &mut self.preview_tex, "photocut_preview", &image);
                    self.preview_meta = Some(PreviewMeta {
                        scan_idx,
                        photo_id,
                        tex_dims,
                        full_dims,
                        show_original,
                    });
                }
                WorkerEvent::BackdropReady {
                    scan_idx,
                    image,
                    orig_dims,
                } => {
                    if self.backdrop_requested_scan == Some(scan_idx) {
                        self.backdrop_requested_scan = None;
                    }
                    self.scan_dims.insert(scan_idx, orig_dims);
                    if let Some((target_scan, target_id)) = self.pending_new_crop_recenter
                        && target_scan == scan_idx
                    {
                        self.pending_new_crop_recenter = None;
                        let recentered = OrientedRect {
                            cx: orig_dims.0 as f32 * 0.5,
                            cy: orig_dims.1 as f32 * 0.5,
                            width: (orig_dims.0 as f32 * 0.45).max(MIN_CROP_SIZE * 2.0),
                            height: (orig_dims.1 as f32 * 0.45).max(MIN_CROP_SIZE * 2.0),
                            angle_deg: 0.0,
                        };
                        if let Some(scan) = self.scans.get_mut(scan_idx)
                            && let Some(photo) = scan.photos.iter_mut().find(|p| p.id == target_id)
                        {
                            photo.crop = recentered;
                            self.original_crops.insert((scan_idx, target_id), recentered);
                            self.save_session();
                        }
                    }
                    if scan_idx == self.selected_scan {
                        upload_rgb_texture(ctx, &mut self.backdrop_tex, "photocut_backdrop", &image);
                        self.backdrop_meta = Some(BackdropMeta {
                            scan_idx,
                            orig_dims,
                        });
                    }
                }
                WorkerEvent::ExportDone {
                    scan_idx,
                    photo_id,
                    output_path,
                    dims,
                    elapsed_ms,
                } => {
                    self.exports_in_flight = self.exports_in_flight.saturating_sub(1);
                    if let Some(scan) = self.scans.get_mut(scan_idx)
                        && let Some(photo) = scan.photos.iter_mut().find(|p| p.id == photo_id)
                    {
                        photo.status = ReviewStatus::Approved;
                    }
                    self.sync_active_job_to_registry();
                    let fname = output_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("foto");
                    self.status_banner = Some(StatusBanner {
                        kind: StatusKind::Success,
                        text: format!(
                            "Guardada {fname} · Foto {} ({} × {} px · {:.0} ms)",
                            photo_id + 1,
                            dims.0,
                            dims.1,
                            elapsed_ms
                        ),
                    });
                }
                WorkerEvent::Error { context, message } => {
                    if context == "Exportar" {
                        self.exports_in_flight = self.exports_in_flight.saturating_sub(1);
                    } else if context == "Re-recortar" {
                        self.backdrop_requested_scan = None;
                    }
                    self.status_banner = Some(StatusBanner {
                        kind: StatusKind::Error,
                        text: format!("{context}: {message}"),
                    });
                }
            }
        }
    }

    fn find_initial_selection(&self) -> Option<(usize, usize)> {
        let list = self.flat_photo_positions();
        list.iter()
            .copied()
            .find(|&(si, pi)| self.scans[si].photos[pi].status == ReviewStatus::Pending)
            .or_else(|| list.first().copied())
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if !matches!(self.screen, Screen::Review { .. })
            || self.input_dir.is_none()
            || ctx.wants_keyboard_input()
        {
            return;
        }

        let space_down = !self.recrop_mode && ctx.input(|i| i.key_down(Key::Space));
        let want_original = !self.recrop_mode && (space_down || self.compare_mouse_held);
        if want_original != self.show_original_held {
            self.show_original_held = want_original;
            self.request_current_preview();
        }

        let (q, e, c, enter, del, left, right, f_key) = ctx.input_mut(|i| {
            let none = egui::Modifiers::NONE;
            (
                i.consume_key(none, Key::Q),
                i.consume_key(none, Key::E),
                i.consume_key(none, Key::C),
                i.consume_key(none, Key::Enter),
                i.consume_key(none, Key::Delete),
                i.consume_key(none, Key::ArrowLeft),
                i.consume_key(none, Key::ArrowRight),
                i.consume_key(none, Key::F),
            )
        });

        if q {
            self.rotate_quarter(false);
        }
        if e {
            self.rotate_quarter(true);
        }
        if c {
            self.toggle_recrop_mode();
        }
        if enter {
            self.approve_and_export_current();
        }
        if del {
            self.skip_current();
        }
        if left {
            self.select_prev();
        }
        if right {
            self.select_next();
        }
        if f_key {
            if self.recrop_mode {
                self.crop_editor.reset_view();
            } else {
                self.reset_view();
            }
        }
    }

    fn photo_status_counts(&self) -> (usize, usize, usize) {
        let mut approved = 0usize;
        let mut skipped = 0usize;
        let mut total = 0usize;
        for scan in &self.scans {
            for p in &scan.photos {
                total += 1;
                match p.status {
                    ReviewStatus::Approved => approved += 1,
                    ReviewStatus::Skipped => skipped += 1,
                    ReviewStatus::Pending => {}
                }
            }
        }
        (approved, skipped, total)
    }

    fn show_ai_badge(&mut self, ui: &mut egui::Ui, clickable: bool) {
        let (ai_label, ai_fill, ai_border, ai_text) = if self.ai_model_loading {
            (
                "IA Orientación: Cargando...",
                theme::BG_CARD,
                theme::BORDER,
                theme::TEXT_SECONDARY,
            )
        } else if !self.ai_orient_available {
            (
                "IA Orientación: No disponible",
                theme::BG_CARD,
                theme::BORDER,
                theme::TEXT_SECONDARY,
            )
        } else if self.ai_orient_enabled {
            (
                "IA Orientación: Activa",
                Color32::from_rgba_unmultiplied(59, 130, 246, 36),
                theme::ACCENT,
                theme::ACCENT_HOVER,
            )
        } else {
            (
                "IA Orientación: Pausada",
                theme::BG_CARD,
                theme::BORDER,
                theme::TEXT_SECONDARY,
            )
        };

        let tooltip = if self.ai_model_loading {
            "Cargando el modelo local que detecta automáticamente si una foto está de costado o al revés.".to_string()
        } else if !self.ai_orient_available {
            format!(
                "El modelo local de orientación automática no está instalado ({}). Igual podés girar cualquier foto manualmente con los botones o teclas Q / E.",
                self.ai_status_detail
            )
        } else if self.ai_orient_enabled {
            format!(
                "Orientación automática por IA activada (100% sin internet, {}). Detecta si las fotos nuevas están giradas o al revés y las pone derechas solas. Hacé clic para desactivarla.",
                self.ai_status_detail
            )
        } else {
            "Orientación automática por IA desactivada. Hacé clic para activarla al analizar nuevos escaneos.".to_string()
        };

        let font_id = egui::FontId::proportional(12.0);
        let galley = ui
            .painter()
            .layout_no_wrap(ai_label.to_string(), font_id.clone(), ai_text);
        let icon_size = 14.0;
        let gap = 6.0;
        let pad_x = 10.0;
        let desired = Vec2::new(icon_size + gap + galley.size().x + pad_x * 2.0, 26.0);
        let can_click = clickable && self.ai_orient_available;
        let (rect, mut resp) = ui.allocate_exact_size(
            desired,
            if can_click { Sense::click() } else { Sense::hover() },
        );
        if can_click && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(13),
            ai_fill,
            theme::stroke(1.0, ai_border),
            StrokeKind::Inside,
        );
        let icon_rect = Rect::from_center_size(
            Pos2::new(rect.min.x + pad_x + icon_size * 0.5, rect.center().y),
            Vec2::splat(icon_size),
        );
        draw_icon(painter, icon_rect, Icon::Sparkles, ai_text);
        painter.galley(
            Pos2::new(
                rect.min.x + pad_x + icon_size + gap,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            ai_text,
        );

        resp = resp.on_hover_text(tooltip);
        if can_click && resp.clicked() {
            self.ai_orient_enabled = !self.ai_orient_enabled;
        }
    }

    fn show_status_banner(&mut self, ui: &mut egui::Ui) {
        let Some(banner) = &self.status_banner else {
            return;
        };
        let (bg, border, fg) = match banner.kind {
            StatusKind::Info => (theme::BG_SURFACE, theme::BORDER, theme::TEXT_SECONDARY),
            StatusKind::Success => (
                Color32::from_rgba_unmultiplied(34, 197, 94, 24),
                theme::APPROVED,
                theme::TEXT_PRIMARY,
            ),
            StatusKind::Error => (
                Color32::from_rgba_unmultiplied(239, 68, 68, 30),
                theme::ERROR,
                theme::TEXT_PRIMARY,
            ),
        };
        let mut dismiss = false;
        Frame::new()
            .fill(bg)
            .stroke(theme::stroke(1.0, border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&banner.text).size(12.5).color(fg));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if subtle_icon_button(ui, Icon::Close, "", "Cerrar aviso", true).clicked() {
                            dismiss = true;
                        }
                    });
                });
            });
        if dismiss {
            self.status_banner = None;
        }
    }

    // =========================================================================
    // PANTALLA 1: MENÚ PRINCIPAL / GESTOR DE TRABAJOS ("Trabajos")
    // =========================================================================

    fn show_main_menu(&mut self, ctx: &egui::Context) {
        // Barra superior limpia del menú principal
        egui::TopBottomPanel::top("menu_top_bar")
            .exact_height(54.0)
            .frame(
                Frame::new()
                    .fill(theme::BG_SURFACE)
                    .inner_margin(Margin::symmetric(18, 8))
                    .stroke(theme::stroke(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("PhotoCut")
                                    .size(17.0)
                                    .strong()
                                    .color(theme::TEXT_PRIMARY),
                            );
                            let version_label = format!("v{}", env!("CARGO_PKG_VERSION"));
                            Frame::new()
                                .fill(theme::BG_CARD)
                                .stroke(theme::stroke(1.0, theme::BORDER))
                                .corner_radius(4.0)
                                .inner_margin(Margin::symmetric(6, 1))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(&version_label)
                                            .size(11.0)
                                            .monospace()
                                            .color(theme::TEXT_SECONDARY),
                                    );
                                })
                                .response
                                .on_hover_text(format!(
                                    "Versión instalada de PhotoCut ({version_label})."
                                ));
                        });
                        ui.label(
                            RichText::new(
                                "Recortá, enderezá y mejorá tus fotos escaneadas en máxima calidad",
                            )
                            .size(12.0)
                            .color(theme::TEXT_SECONDARY),
                        );
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.show_ai_badge(ui, true);
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(theme::BG_VIEWPORT)
                    .inner_margin(Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                let max_w = ui.available_width().min(1040.0);
                let side_pad = ((ui.available_width() - max_w) * 0.5).max(0.0);

                ui.horizontal(|ui| {
                    if side_pad > 0.0 {
                        ui.add_space(side_pad);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(max_w);

                        if self.status_banner.is_some() {
                            self.show_status_banner(ui);
                            ui.add_space(12.0);
                        }

                        self.show_menu_action_bar(ui);
                        ui.add_space(12.0);

                        if self.show_new_job_strip {
                            self.show_new_job_strip_ui(ui);
                            ui.add_space(12.0);
                        }

                        if self.queue_state.is_some() {
                            self.show_active_queue_banner(ui);
                            ui.add_space(12.0);
                        }

                        self.show_jobs_list(ui);
                    });
                });
            });
    }

    fn show_menu_action_bar(&mut self, ui: &mut egui::Ui) {
        let queue_running = self.queue_state.is_some();
        let pending_job_ids: Vec<u64> = self
            .registry
            .jobs
            .iter()
            .filter(|j| j.needs_indexing())
            .map(|j| j.id)
            .collect();

        Frame::new()
            .fill(theme::BG_SURFACE)
            .stroke(theme::stroke(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    if primary_icon_button(
                        ui,
                        Icon::Plus,
                        "Nuevo trabajo",
                        "Creá un trabajo eligiendo una carpeta con escaneos y su carpeta de destino.",
                        Vec2::new(0.0, 32.0),
                        true,
                    )
                    .clicked()
                    {
                        self.show_new_job_strip = !self.show_new_job_strip;
                        if self.show_new_job_strip {
                            self.new_job_draft.use_ai_orient =
                                self.ai_orient_available && self.ai_orient_enabled;
                        }
                    }

                    if icon_button_sized(
                        ui,
                        Icon::FolderStack,
                        "Importar subcarpetas",
                        "Elegí una carpeta principal (por ejemplo 'Escaneo') para agregar cada subcarpeta con imágenes como un trabajo separado.",
                        Vec2::new(0.0, 32.0),
                        !queue_running,
                    )
                    .clicked()
                    {
                        self.import_subfolders_dialog();
                    }

                    if queue_running {
                        let cancelling =
                            self.queue_state.as_ref().is_some_and(|q| q.cancelling);
                        if styled_icon_button(
                            ui,
                            Icon::Pause,
                            if cancelling {
                                "Pausando..."
                            } else {
                                "Pausar cola"
                            },
                            "Detiene el procesamiento al terminar el escaneo actual, guardando todo lo ya detectado sin perder trabajo.",
                            ButtonStyle::Amber,
                            Vec2::new(0.0, 32.0),
                            !cancelling,
                        )
                        .clicked()
                        {
                            self.cancel_job_queue();
                        }
                    } else {
                        let has_pending = !pending_job_ids.is_empty();
                        let tip = if has_pending {
                            "Analiza automáticamente todos los trabajos pendientes uno por uno (podés dejarlo corriendo toda la noche: guarda el progreso después de cada escaneo)."
                        } else {
                            "No hay trabajos pendientes por analizar. Agregá un trabajo nuevo o abrí uno con \"Revisar\"."
                        };
                        if icon_button_sized(
                            ui,
                            Icon::Play,
                            "Procesar todos",
                            tip,
                            Vec2::new(0.0, 32.0),
                            has_pending,
                        )
                        .clicked()
                        {
                            self.start_queue_for_jobs(pending_job_ids);
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if subtle_icon_button(
                            ui,
                            Icon::Refresh,
                            "Actualizar",
                            "Vuelve a revisar las carpetas en el disco para actualizar la cantidad de escaneos y fotos.",
                            !queue_running,
                        )
                        .clicked()
                        {
                            self.registry.refresh_all();
                            self.save_registry();
                        }
                    });
                });
            });
    }

    fn show_new_job_strip_ui(&mut self, ui: &mut egui::Ui) {
        let mut action_create_open = false;
        let mut action_add_queue = false;
        let mut action_cancel = false;

        Frame::new()
            .fill(theme::BG_SURFACE)
            .stroke(theme::stroke(1.0, theme::ACCENT))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;

                    // Fila 1: Nombre + Carpeta de entrada + Carpeta de salida
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;

                        ui.label(
                            RichText::new("Nombre:")
                                .size(12.5)
                                .color(theme::TEXT_SECONDARY),
                        );
                        ui.add(
                            TextEdit::singleline(&mut self.new_job_draft.name)
                                .desired_width(175.0)
                                .hint_text("Opcional (usa la carpeta)"),
                        )
                        .on_hover_text(
                            "Nombre corto para identificar este álbum en tu lista de trabajos (si lo dejás vacío se usa el nombre de la carpeta).",
                        );

                        if icon_button(
                            ui,
                            Icon::FolderOpen,
                            "Elegir carpeta de escaneos",
                            "Seleccioná la carpeta donde guardaste las imágenes escaneadas de este álbum.",
                        )
                        .clicked()
                            && let Some(dir) = rfd::FileDialog::new()
                                .set_title("Seleccionar carpeta de escaneos")
                                .pick_folder()
                        {
                            if self.new_job_draft.name.trim().is_empty() {
                                self.new_job_draft.name = suggest_job_name(&dir);
                            }
                            if self.new_job_draft.output_is_auto
                                || self.new_job_draft.output_dir.is_none()
                            {
                                self.new_job_draft.output_dir = Some(default_output_dir(&dir));
                                self.new_job_draft.output_is_auto = true;
                            }
                            self.new_job_draft.input_dir = Some(dir);
                        }

                        if icon_button(
                            ui,
                            Icon::FolderOutput,
                            "Carpeta de salida",
                            "Carpeta donde se guardarán las fotos finales recortadas (por defecto se crea <carpeta>_recortadas junto a la original).",
                        )
                        .clicked()
                        {
                            let mut dlg = rfd::FileDialog::new()
                                .set_title("Seleccionar carpeta de salida");
                            if let Some(d) = self
                                .new_job_draft
                                .output_dir
                                .as_ref()
                                .or(self.new_job_draft.input_dir.as_ref())
                            {
                                dlg = dlg.set_directory(d);
                            }
                            if let Some(dir) = dlg.pick_folder() {
                                self.new_job_draft.output_dir = Some(dir);
                                self.new_job_draft.output_is_auto = false;
                            }
                        }
                    });

                    // Rutas seleccionadas (si ya eligió carpeta)
                    if let Some(in_dir) = &self.new_job_draft.input_dir {
                        let out_text = self
                            .new_job_draft
                            .output_dir
                            .as_ref()
                            .map(|p| truncate_path(p, 48))
                            .unwrap_or_else(|| "(automática)".to_string());
                        let full_hover = format!(
                            "Entrada: {}\nSalida: {}",
                            in_dir.display(),
                            self.new_job_draft
                                .output_dir
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default()
                        );
                        ui.label(
                            RichText::new(format!(
                                "Entrada: {}   |   Salida: {}",
                                truncate_path(in_dir, 48),
                                out_text
                            ))
                            .size(12.0)
                            .color(theme::TEXT_SECONDARY),
                        )
                        .on_hover_text(full_hover);
                    }

                    // Fila 2: Formato + IA + Botones de confirmación
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;

                        ui.label(
                            RichText::new("Formato:")
                                .size(12.5)
                                .color(theme::TEXT_SECONDARY),
                        );
                        let fmt_label = format_label(self.new_job_draft.export_format);
                        ComboBox::from_id_salt("new_job_format_combo")
                            .selected_text(fmt_label)
                            .width(166.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.new_job_draft.export_format,
                                    ExportFormat::Png,
                                    "PNG (Sin pérdida)",
                                );
                                ui.selectable_value(
                                    &mut self.new_job_draft.export_format,
                                    ExportFormat::Tiff,
                                    "TIFF (Sin pérdida)",
                                );
                                ui.selectable_value(
                                    &mut self.new_job_draft.export_format,
                                    ExportFormat::Jpeg {
                                        quality: self.new_job_draft.jpeg_quality,
                                    },
                                    "JPEG (Alta calidad)",
                                );
                            })
                            .response
                            .on_hover_text(
                                "Elegí el formato en que se guardarán las fotos aprobadas: PNG o TIFF conservan 100% de calidad sin pérdida; JPEG ocupa menos espacio.",
                            );

                        if let ExportFormat::Jpeg { quality } =
                            &mut self.new_job_draft.export_format
                        {
                            let mut q = *quality;
                            if ui
                                .add(
                                    Slider::new(&mut q, 80..=100)
                                        .prefix("Calidad ")
                                        .suffix("%")
                                        .integer(),
                                )
                                .on_hover_text(
                                    "Calidad de compresión JPEG (96% recomendado para máxima nitidez con poco peso).",
                                )
                                .changed()
                            {
                                *quality = q;
                                self.new_job_draft.jpeg_quality = q;
                            }
                        }

                        ui.add_enabled_ui(self.ai_orient_available, |ui| {
                            ui.checkbox(&mut self.new_job_draft.use_ai_orient, "Auto-orientar con IA")
                                .on_hover_text(
                                    "Si está marcado, la IA local detecta si las fotos quedaron de costado o al revés y las gira automáticamente.",
                                )
                                .on_disabled_hover_text(
                                    "El modelo de IA local no está instalado; las fotos se podrán rotar manualmente con Q y E.",
                                );
                        });

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let ready = self.new_job_draft.input_dir.is_some();
                            let queue_busy = self.queue_state.is_some();

                            if primary_icon_button(
                                ui,
                                Icon::Eye,
                                "Crear y abrir",
                                if ready {
                                    "Guarda este trabajo en tu lista y lo abre de inmediato para empezar a revisar fotos."
                                } else {
                                    "Primero elegí una carpeta de escaneos."
                                },
                                Vec2::new(0.0, 30.0),
                                ready && !queue_busy,
                            )
                            .clicked()
                            {
                                action_create_open = true;
                            }

                            if icon_button_sized(
                                ui,
                                Icon::Plus,
                                "Agregar a la cola",
                                if ready {
                                    "Agrega este trabajo a tu lista sin abrirlo todavía, ideal para sumar varios álbumes y procesarlos todos juntos."
                                } else {
                                    "Primero elegí una carpeta de escaneos."
                                },
                                Vec2::new(0.0, 30.0),
                                ready,
                            )
                            .clicked()
                            {
                                action_add_queue = true;
                            }

                            if subtle_icon_button(
                                ui,
                                Icon::Close,
                                "Cancelar",
                                "Cierra el formulario sin agregar el trabajo.",
                                true,
                            )
                            .clicked()
                            {
                                action_cancel = true;
                            }
                        });
                    });
                });
            });

        if action_cancel {
            self.show_new_job_strip = false;
            self.new_job_draft = NewJobDraft::default();
        } else if (action_create_open || action_add_queue)
            && let Some(in_dir) = self.new_job_draft.input_dir.clone()
        {
            let out_dir = self
                .new_job_draft
                .output_dir
                .clone()
                .unwrap_or_else(|| default_output_dir(&in_dir));
            let name = if self.new_job_draft.name.trim().is_empty() {
                suggest_job_name(&in_dir)
            } else {
                self.new_job_draft.name.trim().to_string()
            };
            let job_id = self.registry.add_or_update_job(
                name,
                in_dir,
                out_dir,
                self.new_job_draft.export_format,
                self.new_job_draft.use_ai_orient && self.ai_orient_available,
            );
            self.save_registry();
            self.show_new_job_strip = false;
            self.new_job_draft = NewJobDraft::default();

            if action_create_open {
                self.open_job_for_review(job_id);
            }
        }
    }

    fn show_active_queue_banner(&mut self, ui: &mut egui::Ui) {
        let Some(q) = &self.queue_state else {
            return;
        };
        let job_name = q.job_name.clone();
        let job_num = q.job_index + 1;
        let total_jobs = q.total_jobs.max(1);
        let indexed = q.indexed_scans;
        let total_scans = q.total_scans;
        let total_photos = q.total_photos;
        let scan_name = q.current_scan_name.clone();
        let cancelling = q.cancelling;

        let job_frac = if total_scans > 0 {
            (indexed as f32 / total_scans as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let overall_frac = ((q.job_index as f32 + job_frac) / total_jobs as f32).clamp(0.0, 1.0);

        let mut request_pause = false;

        Frame::new()
            .fill(Color32::from_rgba_unmultiplied(59, 130, 246, 18))
            .stroke(theme::stroke(1.0, theme::ACCENT))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;

                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(15.0));
                        ui.label(
                            RichText::new(format!("Procesando cola: {job_name}"))
                                .size(14.0)
                                .strong()
                                .color(theme::TEXT_PRIMARY),
                        );

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if styled_icon_button(
                                ui,
                                Icon::Pause,
                                if cancelling { "Pausando..." } else { "Pausar" },
                                "Detiene la cola apenas termine el escaneo en curso y guarda todo el avance.",
                                ButtonStyle::Amber,
                                Vec2::new(0.0, 26.0),
                                !cancelling,
                            )
                            .clicked()
                            {
                                request_pause = true;
                            }

                            ui.label(
                                RichText::new(format!(
                                    "Trabajo {job_num} de {total_jobs} · Escaneo {indexed}/{total_scans} · {total_photos} fotos detectadas"
                                ))
                                .size(12.5)
                                .color(theme::TEXT_PRIMARY),
                            );
                        });
                    });

                    draw_progress_bar(ui, overall_frac, 6.0, theme::ACCENT);

                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(
                                "Podés dejar la app corriendo: el progreso se guarda automáticamente después de cada escaneo.",
                            )
                            .size(12.0)
                            .color(theme::TEXT_SECONDARY),
                        );
                        if !scan_name.is_empty() {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(format!("Archivo actual: {scan_name}"))
                                        .size(11.5)
                                        .color(theme::TEXT_SECONDARY),
                                );
                            });
                        }
                    });
                });
            });

        if request_pause {
            self.cancel_job_queue();
        }
    }

    fn show_jobs_list(&mut self, ui: &mut egui::Ui) {
        let ready_count = self
            .registry
            .jobs
            .iter()
            .filter(|j| matches!(j.status, JobStatus::ReadyForReview | JobStatus::Completed))
            .count();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Tus trabajos")
                    .size(16.0)
                    .strong()
                    .color(theme::TEXT_PRIMARY),
            );
            if !self.registry.jobs.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {} · {} listo{}",
                            self.registry.jobs.len(),
                            if self.registry.jobs.len() == 1 {
                                "trabajo"
                            } else {
                                "trabajos"
                            },
                            ready_count,
                            if ready_count == 1 { "" } else { "s" }
                        ))
                        .size(12.0)
                        .color(theme::TEXT_SECONDARY),
                    );
                });
            }
        });
        ui.add_space(8.0);

        if self.registry.jobs.is_empty() {
            self.show_empty_jobs_guide(ui);
            return;
        }

        let queue_running = self.queue_state.is_some();
        let active_queue_job_id = self.queue_state.as_ref().map(|q| q.job_id);
        let jobs_snapshot = self.registry.jobs.clone();

        let mut open_job: Option<u64> = None;
        let mut process_job: Option<u64> = None;
        let mut remove_job: Option<u64> = None;

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for job in &jobs_snapshot {
                    let is_active_in_queue = active_queue_job_id == Some(job.id);
                    let row_action = show_job_row(ui, job, queue_running, is_active_in_queue);
                    match row_action {
                        JobRowAction::Review => open_job = Some(job.id),
                        JobRowAction::Process => process_job = Some(job.id),
                        JobRowAction::Remove => remove_job = Some(job.id),
                        JobRowAction::None => {}
                    }
                }
            });

        if let Some(id) = open_job {
            self.open_job_for_review(id);
        } else if let Some(id) = process_job {
            self.start_queue_for_jobs(vec![id]);
        } else if let Some(id) = remove_job {
            self.registry.remove_job(id);
            self.save_registry();
        }
    }

    fn show_empty_jobs_guide(&mut self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(theme::BG_SURFACE)
            .stroke(theme::stroke(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(28))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Empezá con tu primer álbum de fotos")
                            .size(16.0)
                            .strong()
                            .color(theme::TEXT_PRIMARY),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Podés cargar una carpeta individual o importar varias subcarpetas de una sola vez y dejar PhotoCut procesando mientras hacés otras cosas.",
                        )
                        .size(13.0)
                        .color(theme::TEXT_SECONDARY),
                    );
                    ui.add_space(20.0);

                    workflow_step(
                        ui,
                        "1",
                        "Elegí una o varias carpetas con tus escaneos",
                        "Usá \"Nuevo trabajo\" para una carpeta o \"Importar subcarpetas\" si tenés varios álbumes separados.",
                    );
                    ui.add_space(12.0);
                    workflow_step(
                        ui,
                        "2",
                        "Dejá que PhotoCut recorte y enderece automáticamente",
                        "Detecta cada foto, corrige la inclinación y sugiere la orientación correcta guardando el avance tras cada escaneo.",
                    );
                    ui.add_space(12.0);
                    workflow_step(
                        ui,
                        "3",
                        "Revisá cada álbum con calma y guardá en máxima calidad",
                        "Ajustá bordes o color sin pérdida y pulsá Enter para exportar cada foto aprobada a 600 DPI.",
                    );

                    ui.add_space(24.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        if primary_icon_button(
                            ui,
                            Icon::Plus,
                            "Crear mi primer trabajo",
                            "Abre el formulario para elegir una carpeta con escaneos.",
                            Vec2::new(0.0, 36.0),
                            true,
                        )
                        .clicked()
                        {
                            self.show_new_job_strip = true;
                        }

                        if icon_button_sized(
                            ui,
                            Icon::FolderStack,
                            "Importar subcarpetas",
                            "Elegí una carpeta que contenga varias subcarpetas de álbumes para agregarlas todas juntas.",
                            Vec2::new(0.0, 36.0),
                            true,
                        )
                        .clicked()
                        {
                            self.import_subfolders_dialog();
                        }
                    });
                });
            });
    }

    // =========================================================================
    // PANTALLA 2: VISOR Y REVISIÓN DE FOTOS ("Review")
    // =========================================================================

    fn show_review_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("review_top_bar")
            .exact_height(46.0)
            .frame(
                Frame::new()
                    .fill(theme::BG_SURFACE)
                    .inner_margin(Margin::symmetric(12, 6))
                    .stroke(theme::stroke(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    if icon_button(
                        ui,
                        Icon::Home,
                        "Trabajos",
                        "Volver al menú principal de trabajos (tu progreso actual se guarda automáticamente).",
                    )
                    .clicked()
                    {
                        self.return_to_main_menu();
                        return;
                    }

                    ui.separator();

                    let in_tip = match &self.input_dir {
                        Some(p) => format!(
                            "Cambiar carpeta de escaneos.\nActual: {}",
                            p.display()
                        ),
                        None => "Elegir la carpeta donde están los escaneos originales.".to_string(),
                    };
                    if icon_button(ui, Icon::FolderOpen, "Carpeta entrada", &in_tip).clicked() {
                        self.pick_input_folder_in_review();
                    }

                    let out_tip = match &self.output_dir {
                        Some(p) => format!(
                            "Cambiar carpeta donde se guardan las fotos aprobadas.\nActual: {}",
                            p.display()
                        ),
                        None => {
                            "Elegir la carpeta donde se guardarán las fotos recortadas.".to_string()
                        }
                    };
                    if icon_button(ui, Icon::FolderOutput, "Carpeta salida", &out_tip).clicked() {
                        self.pick_output_folder_in_review();
                    }

                    // Píldora de progreso
                    if self.input_dir.is_some() {
                        ui.separator();
                        let (approved, skipped, total) = self.photo_status_counts();
                        let pending = total.saturating_sub(approved + skipped);
                        Frame::new()
                            .fill(theme::BG_CARD)
                            .corner_radius(CornerRadius::same(12))
                            .inner_margin(Margin::symmetric(10, 4))
                            .stroke(theme::stroke(1.0, theme::BORDER))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    let (dot_rect, _) =
                                        ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                                    ui.painter().circle_filled(
                                        dot_rect.center(),
                                        4.0,
                                        theme::APPROVED,
                                    );
                                    ui.label(
                                        RichText::new(format!("Aprobadas {approved} / {total}"))
                                            .size(12.5)
                                            .color(theme::TEXT_PRIMARY),
                                    );
                                });
                            })
                            .response
                            .on_hover_text(format!(
                                "Aprobadas y guardadas: {approved}\nPendientes por revisar: {pending}\nOmitidas: {skipped}"
                            ));

                        if self.scanning_active {
                            let indexed = self.indexed_flags.iter().filter(|&&f| f).count();
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(
                                RichText::new(format!("Escaneando {indexed}/{}", self.total_scans))
                                    .size(12.0)
                                    .color(theme::TEXT_SECONDARY),
                            )
                            .on_hover_text(
                                "PhotoCut sigue detectando los demás escaneos en segundo plano mientras ya podés revisar las primeras fotos.",
                            );
                        } else if self.exports_in_flight > 0 {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(
                                RichText::new("Guardando 600 DPI...")
                                    .size(12.0)
                                    .color(theme::ACCENT_HOVER),
                            )
                            .on_hover_text(
                                "Exportando la foto aprobada en máxima resolución sin bloquear la pantalla.",
                            );
                        }
                    }

                    // Controles alineados a la derecha: IA + Formato de exportación
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.show_ai_badge(ui, true);

                        ui.separator();

                        if let ExportFormat::Jpeg { quality } = &mut self.export_format {
                            let mut q = *quality;
                            let changed = ui
                                .add(
                                    Slider::new(&mut q, 80..=100)
                                        .prefix("Calidad ")
                                        .suffix("%")
                                        .integer(),
                                )
                                .on_hover_text(
                                    "Calidad del archivo JPEG al guardar (96% ofrece excelente nitidez ocupando mucho menos espacio que PNG).",
                                )
                                .changed();
                            if changed {
                                *quality = q;
                                self.last_jpeg_quality = q;
                                self.save_session();
                                self.sync_active_job_to_registry();
                            }
                        }

                        let fmt_label = format_label(self.export_format);
                        let mut new_format = self.export_format;
                        ComboBox::from_id_salt("export_format_combo")
                            .selected_text(fmt_label)
                            .width(166.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut new_format,
                                    ExportFormat::Png,
                                    "PNG (Sin pérdida)",
                                );
                                ui.selectable_value(
                                    &mut new_format,
                                    ExportFormat::Tiff,
                                    "TIFF (Sin pérdida)",
                                );
                                ui.selectable_value(
                                    &mut new_format,
                                    ExportFormat::Jpeg {
                                        quality: self.last_jpeg_quality,
                                    },
                                    "JPEG (Alta calidad)",
                                );
                            })
                            .response
                            .on_hover_text(
                                "Formato en que se guardará cada foto al pulsar \"OK y Guardar\": PNG o TIFF conservan 100% del detalle sin pérdida; JPEG ocupa menos espacio.",
                            );
                        if new_format != self.export_format {
                            self.export_format = new_format;
                            self.save_session();
                            self.sync_active_job_to_registry();
                        }
                    });
                });
            });
    }

    fn show_left_filmstrip(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("filmstrip_panel")
            .default_width(228.0)
            .width_range(196.0..=300.0)
            .frame(
                Frame::new()
                    .fill(theme::BG_SURFACE)
                    .inner_margin(Margin::same(10))
                    .stroke(theme::stroke(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                let folder_name = self
                    .input_dir
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("Escaneos");
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Fotos del álbum")
                            .size(12.5)
                            .strong()
                            .color(theme::TEXT_PRIMARY),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(folder_name)
                                .size(11.5)
                                .color(theme::TEXT_SECONDARY),
                        );
                    });
                });
                ui.add_space(6.0);
                ui.separator();

                // Botón fijo al pie: Agregar recorte
                egui::TopBottomPanel::bottom("filmstrip_footer")
                    .frame(Frame::new().inner_margin(Margin {
                        top: 8,
                        ..Margin::ZERO
                    }))
                    .show_inside(ui, |ui| {
                        let can_add = !self.scans.is_empty();
                        if icon_button_sized(
                            ui,
                            Icon::Plus,
                            "Agregar recorte",
                            "Si alguna foto no se detectó sola en este escaneo, hacé clic acá para dibujar su recorte manualmente.",
                            Vec2::new(ui.available_width(), 32.0),
                            can_add,
                        )
                        .clicked()
                        {
                            self.add_crop_to_current_scan();
                        }
                    });

                let mut clicked_target: Option<(usize, usize)> = None;
                let mut clicked_empty_scan: Option<usize> = None;

                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        for (si, scan) in self.scans.iter().enumerate() {
                            let stem = scan_short_stem(&scan.rel_path);
                            let is_indexed = self.indexed_flags.get(si).copied().unwrap_or(false);

                            if scan.photos.is_empty() {
                                let is_sel_scan = self.selected_scan == si;
                                let status_lbl = if is_indexed {
                                    "Sin fotos"
                                } else {
                                    "Analizando..."
                                };
                                let tip = if is_indexed {
                                    format!(
                                        "{}: no se detectaron fotos automáticamente. Hacé clic y usá \"Agregar recorte\" si querés recortar manualmente.",
                                        scan.rel_path
                                    )
                                } else {
                                    format!("{}: esperando detección automática.", scan.rel_path)
                                };
                                let row_resp = filmstrip_row(
                                    ui,
                                    &format!("{stem} · {status_lbl}"),
                                    theme::TEXT_SECONDARY,
                                    is_sel_scan,
                                    true,
                                    &tip,
                                );
                                if row_resp.clicked() && is_indexed {
                                    clicked_empty_scan = Some(si);
                                }
                                continue;
                            }

                            for (pi, photo) in scan.photos.iter().enumerate() {
                                let selected =
                                    self.selected_scan == si && self.selected_photo_pos == pi;
                                let (dot_color, status_text) = match photo.status {
                                    ReviewStatus::Pending => {
                                        (theme::TEXT_SECONDARY, "Pendiente de revisar")
                                    }
                                    ReviewStatus::Approved => {
                                        (theme::APPROVED, "Aprobada y guardada")
                                    }
                                    ReviewStatus::Skipped => (theme::SKIPPED, "Omitida"),
                                };
                                let (w, h) = full_output_dims(photo.crop, photo.turn);
                                let label = format!("{stem} · Foto {}", photo.id + 1);
                                let tip = format!(
                                    "{} · Foto {}\nEstado: {status_text}\nResolución final: {w} × {h} px",
                                    scan.rel_path,
                                    photo.id + 1
                                );
                                let muted = photo.status == ReviewStatus::Skipped;
                                let resp =
                                    filmstrip_row(ui, &label, dot_color, selected, muted, &tip);
                                if resp.clicked() {
                                    clicked_target = Some((si, pi));
                                }
                            }
                        }
                    });

                if let Some((si, pi)) = clicked_target {
                    self.select_photo(si, pi);
                } else if let Some(si) = clicked_empty_scan {
                    self.selected_scan = si;
                    self.selected_photo_pos = 0;
                    self.preview_tex = None;
                    self.preview_meta = None;
                    self.request_current_backdrop();
                }
            });
    }

    fn show_right_inspector(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("inspector_panel")
            .default_width(296.0)
            .width_range(272.0..=340.0)
            .frame(
                Frame::new()
                    .fill(theme::BG_SURFACE)
                    .inner_margin(Margin::same(12))
                    .stroke(theme::stroke(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                let has_photo = self.current_photo().is_some();

                // Acciones fijas al pie del panel derecho
                egui::TopBottomPanel::bottom("inspector_actions_footer")
                    .frame(
                        Frame::new()
                            .fill(theme::BG_SURFACE)
                            .inner_margin(Margin {
                                top: 10,
                                ..Margin::ZERO
                            }),
                    )
                    .show_inside(ui, |ui| {
                        ui.separator();
                        ui.add_space(8.0);

                        if primary_icon_button(
                            ui,
                            Icon::Check,
                            "OK y Guardar  [Enter]",
                            "Guarda esta foto en máxima resolución (600 DPI) en tu carpeta de salida y pasa automáticamente a la siguiente.",
                            Vec2::new(ui.available_width(), 38.0),
                            has_photo,
                        )
                        .clicked()
                        {
                            self.approve_and_export_current();
                        }

                        ui.add_space(6.0);

                        if icon_button_sized(
                            ui,
                            Icon::Skip,
                            "Omitir  [Del]",
                            "Salta esta foto sin guardarla y pasa a la siguiente (podés volver a revisarla después si cambiás de idea).",
                            Vec2::new(ui.available_width(), 30.0),
                            has_photo,
                        )
                        .clicked()
                        {
                            self.skip_current();
                        }

                        ui.add_space(6.0);

                        let avail = ui.available_width();
                        let half = (avail - 6.0) * 0.5;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if icon_button_sized(
                                ui,
                                Icon::ChevronLeft,
                                "Anterior",
                                "Vuelve a la foto anterior de la lista (flecha izquierda).",
                                Vec2::new(half, 30.0),
                                has_photo,
                            )
                            .clicked()
                            {
                                self.select_prev();
                            }
                            if icon_button_sized(
                                ui,
                                Icon::ChevronRight,
                                "Siguiente",
                                "Avanza a la siguiente foto sin guardar cambios todavía (flecha derecha).",
                                Vec2::new(half, 30.0),
                                has_photo,
                            )
                            .clicked()
                            {
                                self.select_next();
                            }
                        });
                    });

                // Cuerpo desplazable del inspector
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let Some(mut photo) = self.current_photo().cloned() else {
                            ui.add_space(24.0);
                            ui.label(
                                RichText::new("Seleccioná una foto para ver sus controles.")
                                    .color(theme::TEXT_SECONDARY),
                            );
                            return;
                        };

                        let mut preview_dirty = false;
                        let mut save_needed = false;

                        // === SECCIÓN 1: POSICIÓN Y RECORTE ===
                        section_heading(ui, "Posición y recorte");
                        ui.add_space(6.0);

                        let avail = ui.available_width();
                        let half = (avail - 6.0) * 0.5;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if icon_button_sized(
                                ui,
                                Icon::RotateLeft,
                                "90° Izq [Q]",
                                "Gira la foto 90 grados hacia la izquierda (tecla Q).",
                                Vec2::new(half, 30.0),
                                true,
                            )
                            .clicked()
                            {
                                photo.turn = photo.turn.rotate_ccw();
                                preview_dirty = true;
                                save_needed = true;
                            }
                            if icon_button_sized(
                                ui,
                                Icon::RotateRight,
                                "90° Der [E]",
                                "Gira la foto 90 grados hacia la derecha (tecla E).",
                                Vec2::new(half, 30.0),
                                true,
                            )
                            .clicked()
                            {
                                photo.turn = photo.turn.rotate_cw();
                                preview_dirty = true;
                                save_needed = true;
                            }
                        });

                        ui.add_space(10.0);

                        // Ángulo fino (-15°..+15°) + reset 0°
                        let angle_tip = "Endereza inclinaciones pequeñas si la foto quedó levemente torcida al apoyarla en el escáner.";
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Ángulo fino")
                                    .size(12.5)
                                    .color(theme::TEXT_SECONDARY),
                            )
                            .on_hover_text(angle_tip);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if small_icon_button(
                                    ui,
                                    Icon::Reset,
                                    "0°",
                                    "Deja la inclinación fina exactamente en 0 grados.",
                                )
                                .clicked()
                                {
                                    photo.crop.angle_deg = 0.0;
                                    preview_dirty = true;
                                    save_needed = true;
                                }
                                ui.label(
                                    RichText::new(format!("{:+.1}°", photo.crop.angle_deg))
                                        .monospace()
                                        .size(12.0)
                                        .color(theme::TEXT_PRIMARY),
                                );
                            });
                        });

                        let angle_resp = ui
                            .add_sized(
                                [ui.available_width(), 20.0],
                                Slider::new(&mut photo.crop.angle_deg, -15.0..=15.0)
                                    .clamping(SliderClamping::Never)
                                    .step_by(0.1)
                                    .show_value(false),
                            )
                            .on_hover_text(angle_tip);
                        if angle_resp.changed() {
                            preview_dirty = true;
                        }
                        if angle_resp.drag_stopped() {
                            save_needed = true;
                        }

                        ui.add_space(10.0);

                        // Botón destacado Re-recortar [C]
                        let recrop_label = if self.recrop_mode {
                            "Finalizar Re-recorte  [C]"
                        } else {
                            "Re-recortar  [C]"
                        };
                        let recrop_style = if self.recrop_mode {
                            ButtonStyle::AmberFilled
                        } else {
                            ButtonStyle::Amber
                        };
                        let toggle_recrop = styled_icon_button(
                            ui,
                            Icon::Crop,
                            recrop_label,
                            "Abre el escaneo completo para agrandar, mover o corregir los bordes del recorte sin perder calidad (tecla C).",
                            recrop_style,
                            Vec2::new(ui.available_width(), 34.0),
                            true,
                        )
                        .clicked();

                        ui.add_space(16.0);
                        ui.separator();
                        ui.add_space(12.0);

                        // === SECCIÓN 2: COLOR Y LUMINOSIDAD ===
                        section_heading(ui, "Color y luminosidad");
                        ui.add_space(8.0);

                        let adj = &mut photo.adjustments;
                        let mut auto_pct = adj.auto_color_strength * 100.0;
                        let (c, s) = labeled_slider(
                            ui,
                            "Auto-Color",
                            "Corrige automáticamente fotos viejas desteñidas o rosadas y mejora el contraste.",
                            &mut auto_pct,
                            0.0..=100.0,
                            "%",
                            true,
                        );
                        if c {
                            adj.auto_color_strength = (auto_pct / 100.0).clamp(0.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut bri = adj.brightness * 100.0;
                        let (c, s) = labeled_slider(
                            ui,
                            "Brillo",
                            "Aclara u oscurece toda la foto.",
                            &mut bri,
                            -100.0..=100.0,
                            "",
                            false,
                        );
                        if c {
                            adj.brightness = (bri / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut con = adj.contrast * 100.0;
                        let (c, s) = labeled_slider(
                            ui,
                            "Contraste",
                            "Aumenta o suaviza la diferencia entre sombras y luces.",
                            &mut con,
                            -100.0..=100.0,
                            "",
                            false,
                        );
                        if c {
                            adj.contrast = (con / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut sat = adj.saturation * 100.0;
                        let (c, s) = labeled_slider(
                            ui,
                            "Saturación",
                            "Hace los colores más vivos o más suaves.",
                            &mut sat,
                            -100.0..=100.0,
                            "",
                            false,
                        );
                        if c {
                            adj.saturation = (sat / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut temp = adj.temperature * 100.0;
                        let (c, s) = labeled_slider(
                            ui,
                            "Temperatura",
                            "Hacia la izquierda enfría la foto (más azul); hacia la derecha la entibia (más cálida).",
                            &mut temp,
                            -100.0..=100.0,
                            "",
                            false,
                        );
                        if c {
                            adj.temperature = (temp / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut tint = adj.tint * 100.0;
                        let (c, s) = labeled_slider(
                            ui,
                            "Tinte",
                            "Corrige tonos verdosos o rosados/magenta típicos de fotos antiguas.",
                            &mut tint,
                            -100.0..=100.0,
                            "",
                            false,
                        );
                        if c {
                            adj.tint = (tint / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        ui.add_space(8.0);
                        if icon_button_sized(
                            ui,
                            Icon::Reset,
                            "Restablecer color",
                            "Vuelve todos los controles de color y luz a los valores recomendados por defecto.",
                            Vec2::new(ui.available_width(), 28.0),
                            true,
                        )
                        .clicked()
                        {
                            photo.adjustments = Adjustments::default();
                            preview_dirty = true;
                            save_needed = true;
                        }

                        if preview_dirty || save_needed {
                            if let Some(target) = self.current_photo_mut() {
                                *target = photo;
                            }
                            if preview_dirty && !self.recrop_mode {
                                self.request_current_preview();
                            }
                            if save_needed {
                                self.save_session();
                            }
                        }

                        if toggle_recrop {
                            self.toggle_recrop_mode();
                        }
                    });
            });
    }

    fn show_center_viewport(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG_VIEWPORT))
            .show(ctx, |ui| {
                if self.status_banner.is_some() {
                    Frame::new()
                        .inner_margin(Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            self.show_status_banner(ui);
                        });
                }

                if self.recrop_mode {
                    self.show_recrop_viewport(ui);
                } else {
                    self.show_review_viewport(ui);
                }
            });
    }

    fn show_recrop_viewport(&mut self, ui: &mut egui::Ui) {
        let mut margin_delta: Option<f32> = None;
        let mut restore_original = false;
        let mut reset_sheet_view = false;
        let mut focus_photo_view = false;
        let mut finish_recrop = false;

        let backdrop_ready = self
            .backdrop_meta
            .as_ref()
            .filter(|m| m.scan_idx == self.selected_scan)
            .map(|m| m.orig_dims);

        Frame::new()
            .fill(theme::BG_SURFACE)
            .stroke(theme::stroke(1.0, theme::BORDER))
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;

                        Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(34, 197, 94, 28))
                            .stroke(theme::stroke(1.0, theme::APPROVED))
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    let (r, _) =
                                        ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
                                    draw_icon(ui.painter(), r, Icon::Crop, theme::APPROVED);
                                    ui.label(
                                        RichText::new("RE-RECORTAR")
                                            .size(11.5)
                                            .strong()
                                            .color(theme::APPROVED),
                                    );
                                });
                            });

                        if icon_button(
                            ui,
                            Icon::ZoomFit,
                            "Hoja completa [F]",
                            "Muestra toda la hoja escaneada para ver todas las fotos y márgenes (tecla F).",
                        )
                        .clicked()
                        {
                            reset_sheet_view = true;
                        }

                        let can_focus =
                            backdrop_ready.is_some() && self.crop_editor.last_viewport().is_some();
                        if icon_button_sized(
                            ui,
                            Icon::Eye,
                            "Enfocar foto",
                            "Acerca cómodamente la vista sobre la foto seleccionada dejando margen gris alrededor.",
                            Vec2::new(0.0, 28.0),
                            can_focus,
                        )
                        .clicked()
                        {
                            focus_photo_view = true;
                        }

                        ui.separator();

                        if icon_button(
                            ui,
                            Icon::Expand,
                            "+20 px",
                            "Agranda el recorte 20 píxeles por lado para recuperar partes de la foto que se hayan cortado de más.",
                        )
                        .clicked()
                        {
                            margin_delta = Some(20.0);
                        }
                        if icon_button(
                            ui,
                            Icon::Shrink,
                            "-20 px",
                            "Achica el recorte 20 píxeles por lado para quitar filos blancos del borde del escáner.",
                        )
                        .clicked()
                        {
                            margin_delta = Some(-20.0);
                        }
                        if icon_button(
                            ui,
                            Icon::Reset,
                            "Restaurar recorte",
                            "Vuelve al recorte original detectado automáticamente por PhotoCut.",
                        )
                        .clicked()
                        {
                            restore_original = true;
                        }

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if styled_icon_button(
                                ui,
                                Icon::Check,
                                "Listo  [C]",
                                "Termina de ajustar el recorte y vuelve a la vista previa normal (tecla C).",
                                ButtonStyle::GreenFilled,
                                Vec2::new(98.0, 28.0),
                                true,
                            )
                            .clicked()
                            {
                                finish_recrop = true;
                            }
                        });
                    });

                    ui.label(
                        RichText::new(
                            "Arrastrá cualquier esquina o borde del rectángulo verde para ajustar el recorte. Rueda del mouse para hacer zoom; arrastrá fuera del recorte para mover la hoja.",
                        )
                        .size(11.5)
                        .color(theme::TEXT_SECONDARY),
                    );
                });
            });

        if reset_sheet_view {
            self.crop_editor.reset_view();
        }
        if focus_photo_view
            && let (Some(orig_dims), Some(viewport), Some(crop)) = (
                backdrop_ready,
                self.crop_editor.last_viewport(),
                self.current_photo().map(|p| p.crop),
            )
        {
            self.crop_editor.focus_on_crop(viewport, orig_dims, crop);
        }

        if let Some(delta) = margin_delta
            && let Some(photo) = self.current_photo_mut()
        {
            let expanded = photo.crop.expanded(delta);
            photo.crop.width = expanded.width.max(MIN_CROP_SIZE);
            photo.crop.height = expanded.height.max(MIN_CROP_SIZE);
            self.save_session();
        }

        if restore_original
            && let Some(photo) = self.current_photo().cloned()
            && let Some(&orig) = self.original_crops.get(&(self.selected_scan, photo.id))
            && let Some(target) = self.current_photo_mut()
        {
            target.crop = orig;
            self.save_session();
        }

        if finish_recrop {
            self.toggle_recrop_mode();
            return;
        }

        let footer_text = self.current_photo().map(|p| {
            format!(
                "Recorte original: {} × {} px · Ángulo: {:+.1}°",
                p.crop.width.round() as u32,
                p.crop.height.round() as u32,
                p.crop.angle_deg
            )
        });
        let recrop_zoom_pct = (self.crop_editor.zoom * 100.0).round() as u32;

        egui::TopBottomPanel::bottom("recrop_footer")
            .exact_height(30.0)
            .frame(
                Frame::new()
                    .fill(theme::BG_SURFACE)
                    .stroke(theme::stroke(1.0, theme::BORDER))
                    .inner_margin(Margin::symmetric(12, 5)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if let Some(txt) = &footer_text {
                        ui.label(RichText::new(txt).size(12.0).color(theme::TEXT_SECONDARY));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("Zoom: {recrop_zoom_pct}%"))
                                .monospace()
                                .size(12.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                    });
                });
            });

        match (backdrop_ready, &self.backdrop_tex) {
            (Some(orig_dims), Some(tex)) => {
                let Some(mut crop) = self.current_photo().map(|p| p.crop) else {
                    return;
                };
                let current_id = self.current_photo().map(|p| p.id).unwrap_or(0);
                let siblings: Vec<(usize, OrientedRect)> = self
                    .scans
                    .get(self.selected_scan)
                    .map(|s| {
                        s.photos
                            .iter()
                            .filter(|p| p.id != current_id)
                            .map(|p| (p.id, p.crop))
                            .collect()
                    })
                    .unwrap_or_default();

                let outcome = show_crop_editor(
                    ui,
                    &mut self.crop_editor,
                    tex,
                    orig_dims,
                    &mut crop,
                    &siblings,
                );

                if outcome.changed
                    && let Some(target) = self.current_photo_mut()
                {
                    target.crop = crop;
                }
                if outcome.drag_finished {
                    self.save_session();
                }
            }
            _ => {
                self.request_current_backdrop();
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(18.0));
                        ui.label(
                            RichText::new("Cargando escaneo completo...")
                                .color(theme::TEXT_SECONDARY),
                        );
                    });
                });
            }
        }
    }

    fn show_review_viewport(&mut self, ui: &mut egui::Ui) {
        let current_photo = self.current_photo().cloned();
        let scan_label = self
            .scans
            .get(self.selected_scan)
            .map(|s| {
                let name = Path::new(&s.rel_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&s.rel_path);
                let count = s.photos.len();
                let pos = (self.selected_photo_pos + 1).min(count);
                format!("{name} · Foto {pos} de {count}")
            })
            .unwrap_or_default();

        let dims_label = current_photo
            .as_ref()
            .map(|p| {
                let (w, h) = self
                    .preview_meta
                    .as_ref()
                    .filter(|m| m.scan_idx == self.selected_scan && m.photo_id == p.id)
                    .map(|m| m.full_dims)
                    .unwrap_or_else(|| full_output_dims(p.crop, p.turn));
                format!("{w} × {h} px · 600 DPI")
            })
            .unwrap_or_default();

        egui::TopBottomPanel::bottom("review_footer")
            .exact_height(34.0)
            .frame(
                Frame::new()
                    .fill(theme::BG_SURFACE)
                    .stroke(theme::stroke(1.0, theme::BORDER))
                    .inner_margin(Margin::symmetric(12, 5)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if !dims_label.is_empty() {
                        ui.label(
                            RichText::new(&dims_label)
                                .monospace()
                                .size(12.0)
                                .color(theme::TEXT_PRIMARY),
                        )
                        .on_hover_text(
                            "Resolución real en píxeles con la que se guardará esta foto al aprobarla.",
                        );
                        ui.separator();
                    }
                    if !scan_label.is_empty() {
                        ui.label(
                            RichText::new(&scan_label)
                                .size(12.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if small_icon_button(
                            ui,
                            Icon::ZoomFit,
                            "Ajustar vista [F]",
                            "Vuelve a encuadrar la foto completa en pantalla (también podés usar la rueda del mouse para acercar o alejar).",
                        )
                        .clicked()
                        {
                            self.reset_view();
                        }

                        ui.label(
                            RichText::new(format!("{:.0}%", self.zoom * 100.0))
                                .monospace()
                                .size(12.0)
                                .color(theme::TEXT_SECONDARY),
                        )
                        .on_hover_text("Nivel de zoom actual en la vista previa.");

                        ui.separator();

                        // Botón interactivo / pista de Antes y Después
                        let compare_resp = styled_icon_button(
                            ui,
                            Icon::Eye,
                            "Antes / Después [Espacio]",
                            "Mantené presionado este botón o la barra Espaciadora para ver la foto original sin ajustes de color y compararla.",
                            if self.show_original_held {
                                ButtonStyle::Amber
                            } else {
                                ButtonStyle::Subtle
                            },
                            Vec2::new(0.0, 22.0),
                            current_photo.is_some(),
                        );
                        self.compare_mouse_held = compare_resp.is_pointer_button_down_on();
                        if self.compare_mouse_held && !self.show_original_held {
                            self.show_original_held = true;
                            self.request_current_preview();
                        }
                    });
                });
            });

        let available = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(available, Sense::click_and_drag());

        if response.hovered() {
            let scroll_y = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if scroll_y.abs() > 0.1 {
                let factor = (1.0 + scroll_y * 0.0022).clamp(0.75, 1.35);
                self.zoom = (self.zoom * factor).clamp(0.25, 8.0);
                if (self.zoom - 1.0).abs() < 0.02 {
                    self.zoom = 1.0;
                    self.pan = Vec2::ZERO;
                }
            }
        }
        if response.dragged() {
            self.pan += response.drag_delta();
        }
        if response.double_clicked() {
            self.reset_view();
        }

        let painter = ui.painter_at(available);
        painter.rect_filled(available, CornerRadius::ZERO, theme::BG_VIEWPORT);

        if let (Some(tex), Some(meta)) = (&self.preview_tex, &self.preview_meta) {
            let padding = 24.0;
            let fit_w = (available.width() - padding * 2.0).max(32.0);
            let fit_h = (available.height() - padding * 2.0).max(32.0);
            let img_w = (meta.tex_dims.0 as f32).max(1.0);
            let img_h = (meta.tex_dims.1 as f32).max(1.0);
            let base_scale = (fit_w / img_w).min(fit_h / img_h);
            let disp_size =
                Vec2::new(img_w * base_scale * self.zoom, img_h * base_scale * self.zoom);
            let img_rect = Rect::from_center_size(available.center() + self.pan, disp_size);

            painter.image(
                tex.id(),
                img_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.rect_stroke(
                img_rect,
                CornerRadius::ZERO,
                theme::stroke(1.0, theme::BORDER),
                StrokeKind::Outside,
            );

            if self.show_original_held || meta.show_original {
                let badge_pos = available.min + Vec2::new(16.0, 16.0);
                let label = "ANTES (Original)";
                let galley = painter.layout_no_wrap(
                    label.to_string(),
                    egui::FontId::proportional(12.5),
                    theme::BG_VIEWPORT,
                );
                let badge_rect =
                    Rect::from_min_size(badge_pos, galley.size() + Vec2::new(20.0, 10.0));
                painter.rect_filled(badge_rect, CornerRadius::same(6), theme::RECROP);
                painter.galley(badge_pos + Vec2::new(10.0, 5.0), galley, theme::BG_VIEWPORT);
            }
        } else if self.scanning_active {
            painter.text(
                available.center(),
                egui::Align2::CENTER_CENTER,
                "Analizando primer escaneo...",
                egui::FontId::proportional(14.0),
                theme::TEXT_SECONDARY,
            );
        } else {
            let msg = if self.scans.is_empty() {
                "No se encontraron escaneos en la carpeta."
            } else {
                "No hay fotos detectadas en este escaneo. Usá \"Agregar recorte\" para crear una."
            };
            painter.text(
                available.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(13.5),
                theme::TEXT_SECONDARY,
            );
        }
    }
}

impl eframe::App for PhotoCutApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker_events(ctx);
        self.handle_shortcuts(ctx);

        match self.screen {
            Screen::MainMenu => self.show_main_menu(ctx),
            Screen::Review { .. } => {
                self.show_review_top_bar(ctx);
                if matches!(self.screen, Screen::Review { .. }) {
                    self.show_left_filmstrip(ctx);
                    self.show_right_inspector(ctx);
                    self.show_center_viewport(ctx);
                }
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.sync_active_job_to_registry();
        self.worker.shutdown();
    }
}

enum JobRowAction {
    None,
    Review,
    Process,
    Remove,
}

fn show_job_row(
    ui: &mut egui::Ui,
    job: &JobRecord,
    queue_running: bool,
    is_active_in_queue: bool,
) -> JobRowAction {
    let mut action = JobRowAction::None;

    let border_color = if is_active_in_queue {
        theme::ACCENT
    } else {
        theme::BORDER
    };

    Frame::new()
        .fill(theme::BG_SURFACE)
        .stroke(theme::stroke(1.0, border_color))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;

                // Línea 1: Nombre + Píldora de estado + Botones de acción
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    ui.label(
                        RichText::new(&job.name)
                            .size(14.5)
                            .strong()
                            .color(theme::TEXT_PRIMARY),
                    );

                    show_job_status_pill(ui, &job.status, is_active_in_queue);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;

                        if danger_icon_button(
                            ui,
                            Icon::Trash,
                            "Quitar",
                            "Solo quita el trabajo de esta lista; tus escaneos y fotos guardadas no se borran del disco.",
                            !queue_running,
                        )
                        .clicked()
                        {
                            action = JobRowAction::Remove;
                        }

                        if job.needs_indexing() {
                            if icon_button_sized(
                                ui,
                                Icon::Play,
                                "Procesar",
                                "Detecta y endereza automáticamente todas las fotos de este trabajo en segundo plano para dejarlo listo para revisar.",
                                Vec2::new(0.0, 28.0),
                                !queue_running,
                            )
                            .clicked()
                            {
                                action = JobRowAction::Process;
                            }
                        }

                        let review_primary = matches!(
                            job.status,
                            JobStatus::ReadyForReview | JobStatus::Completed
                        );
                        let review_tip = if queue_running {
                            "Esperá a que termine la cola o pulsá \"Pausar cola\" para abrir este trabajo (tu progreso ya está guardado)."
                        } else {
                            "Abre este trabajo para revisar cada foto, corregir bordes o color y guardar las finales en máxima calidad."
                        };
                        let review_resp = if review_primary {
                            primary_icon_button(
                                ui,
                                Icon::Eye,
                                "Revisar",
                                review_tip,
                                Vec2::new(0.0, 28.0),
                                !queue_running,
                            )
                        } else {
                            icon_button_sized(
                                ui,
                                Icon::Eye,
                                "Revisar",
                                review_tip,
                                Vec2::new(0.0, 28.0),
                                !queue_running,
                            )
                        };
                        if review_resp.clicked() {
                            action = JobRowAction::Review;
                        }
                    });
                });

                // Línea 2: Rutas de entrada y salida (truncadas con ruta completa en tooltip)
                let paths_text = format!(
                    "Entrada: {}   |   Salida: {}",
                    truncate_path(&job.input_dir, 46),
                    truncate_path(&job.output_dir, 46)
                );
                let paths_tooltip = format!(
                    "Carpeta de escaneos:\n{}\n\nCarpeta donde se guardan las fotos recortadas:\n{}",
                    job.input_dir.display(),
                    job.output_dir.display()
                );
                ui.label(
                    RichText::new(paths_text)
                        .size(12.0)
                        .color(theme::TEXT_SECONDARY),
                )
                .on_hover_text(paths_tooltip);

                // Línea 3: Contadores + barra de progreso compacta
                let pending = job.pending_photos();
                let fmt_short = format_short_label(job.export_format);
                let counters = format!(
                    "Escaneos: {}/{} · Fotos: {} ({} aprobadas · {} pendientes) · {}",
                    job.indexed_scans,
                    job.total_scans,
                    job.total_photos,
                    job.approved_photos,
                    pending,
                    fmt_short
                );
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(counters)
                            .size(12.0)
                            .color(theme::TEXT_SECONDARY),
                    );
                    if let JobStatus::Error(err) = &job.status {
                        ui.label(RichText::new(format!("· {err}")).size(12.0).color(theme::ERROR));
                    }
                });

                let (frac, bar_color) = if job.needs_indexing() {
                    let f = if job.total_scans > 0 {
                        (job.indexed_scans as f32 / job.total_scans as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    (f, theme::ACCENT)
                } else if job.total_photos > 0 {
                    let reviewed = job.approved_photos + job.skipped_photos;
                    let f = (reviewed as f32 / job.total_photos as f32).clamp(0.0, 1.0);
                    let col = if reviewed >= job.total_photos {
                        theme::APPROVED
                    } else {
                        theme::RECROP
                    };
                    (f, col)
                } else {
                    (0.0, theme::BORDER)
                };

                draw_progress_bar(ui, frac, 4.0, bar_color);
            });
        });

    action
}

fn show_job_status_pill(ui: &mut egui::Ui, status: &JobStatus, active_spinner: bool) {
    let (label, dot_color, fill, tip) = match status {
        JobStatus::Pending => (
            "Pendiente",
            theme::TEXT_SECONDARY,
            theme::BG_CARD,
            "Todavía faltan escaneos por analizar. Podés procesarlo en la cola o abrirlo directamente con \"Revisar\".",
        ),
        JobStatus::Processing => (
            "Procesando...",
            theme::ACCENT_HOVER,
            Color32::from_rgba_unmultiplied(59, 130, 246, 30),
            "Detectando y enderezando las fotos de esta carpeta en segundo plano.",
        ),
        JobStatus::ReadyForReview => (
            "Listo para revisar",
            theme::RECROP,
            Color32::from_rgba_unmultiplied(245, 158, 11, 24),
            "Todos los escaneos ya fueron analizados. Hacé clic en \"Revisar\" para aprobar y guardar tus fotos.",
        ),
        JobStatus::Completed => (
            "Completado",
            theme::APPROVED,
            Color32::from_rgba_unmultiplied(34, 197, 94, 24),
            "Todas las fotos de este álbum ya fueron revisadas. Podés volver a abrirlo si querés re-editar alguna.",
        ),
        JobStatus::Error(_) => (
            "Error",
            theme::ERROR,
            Color32::from_rgba_unmultiplied(239, 68, 68, 28),
            "Hubo un problema al leer esta carpeta. Verificá que la ruta exista y tenga imágenes JPG, PNG o TIFF.",
        ),
    };

    Frame::new()
        .fill(fill)
        .stroke(theme::stroke(1.0, dot_color))
        .corner_radius(CornerRadius::same(11))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if active_spinner || matches!(status, JobStatus::Processing) {
                    ui.add(egui::Spinner::new().size(10.0));
                } else {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(7.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.5, dot_color);
                }
                ui.label(RichText::new(label).size(11.5).color(theme::TEXT_PRIMARY));
            });
        })
        .response
        .on_hover_text(tip);
}

fn draw_progress_bar(ui: &mut egui::Ui, frac: f32, height: f32, fill_color: Color32) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    let r = CornerRadius::same((height * 0.5).ceil() as u8);
    painter.rect_filled(rect, r, theme::BG_CARD);
    let clamped = frac.clamp(0.0, 1.0);
    if clamped > 0.001 {
        let fill_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width() * clamped, rect.height()));
        painter.rect_filled(fill_rect, r, fill_color);
    }
}

/// Sube una imagen RGB al único `TextureHandle` reutilizable (cero fugas de VRAM).
fn upload_rgb_texture(
    ctx: &egui::Context,
    slot: &mut Option<TextureHandle>,
    name: &str,
    img: &RgbImage,
) {
    let color_img = egui::ColorImage::from_rgb(
        [img.width() as usize, img.height() as usize],
        img.as_raw(),
    );
    match slot {
        Some(tex) => tex.set(color_img, TextureOptions::LINEAR),
        None => *slot = Some(ctx.load_texture(name, color_img, TextureOptions::LINEAR)),
    }
}

fn format_label(fmt: ExportFormat) -> &'static str {
    match fmt {
        ExportFormat::Png => "PNG (Sin pérdida)",
        ExportFormat::Tiff => "TIFF (Sin pérdida)",
        ExportFormat::Jpeg { .. } => "JPEG (Alta calidad)",
    }
}

fn format_short_label(fmt: ExportFormat) -> String {
    match fmt {
        ExportFormat::Png => "PNG".to_string(),
        ExportFormat::Tiff => "TIFF".to_string(),
        ExportFormat::Jpeg { quality } => format!("JPEG {quality}%"),
    }
}

fn truncate_path(path: &Path, max_chars: usize) -> String {
    let s = path.display().to_string();
    let count = s.chars().count();
    if count <= max_chars {
        return s;
    }
    let tail: String = s
        .chars()
        .skip(count.saturating_sub(max_chars.saturating_sub(3)))
        .collect();
    format!("...{tail}")
}

fn build_export_path(
    output_dir: &Path,
    rel_path: &str,
    photo_id: usize,
    format: ExportFormat,
) -> PathBuf {
    let rel = Path::new(rel_path);
    let stem = rel
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("foto");
    let filename = format!("{stem}_foto{:02}.{}", photo_id + 1, format.extension());
    match rel.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => output_dir.join(parent).join(filename),
        _ => output_dir.join(filename),
    }
}

fn scan_short_stem(rel_path: &str) -> &str {
    Path::new(rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(rel_path)
}

fn section_heading(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .size(13.0)
            .strong()
            .color(theme::TEXT_PRIMARY),
    );
}

fn workflow_step(ui: &mut egui::Ui, num: &str, title: &str, desc: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 11.5, theme::BG_CARD);
        ui.painter().circle_stroke(
            rect.center(),
            11.5,
            theme::stroke(1.0, theme::ACCENT),
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            num,
            egui::FontId::proportional(12.0),
            theme::ACCENT_HOVER,
        );

        ui.vertical(|ui| {
            ui.label(
                RichText::new(title)
                    .size(13.5)
                    .strong()
                    .color(theme::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(desc)
                    .size(12.5)
                    .color(theme::TEXT_SECONDARY),
            );
        });
    });
}

fn filmstrip_row(
    ui: &mut egui::Ui,
    label: &str,
    dot_color: Color32,
    selected: bool,
    muted: bool,
    tooltip: &str,
) -> egui::Response {
    let desired = Vec2::new(ui.available_width(), 28.0);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let bg = if selected {
        Color32::from_rgba_unmultiplied(59, 130, 246, 26)
    } else if response.hovered() {
        theme::BG_CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };
    let border = if selected {
        theme::ACCENT
    } else {
        Color32::TRANSPARENT
    };

    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(5),
        bg,
        theme::stroke(1.0, border),
        StrokeKind::Inside,
    );

    let dot_center = Pos2::new(rect.min.x + 13.0, rect.center().y);
    painter.circle_filled(dot_center, 4.0, dot_color);

    let text_color = if selected {
        theme::TEXT_PRIMARY
    } else if muted {
        theme::SKIPPED
    } else {
        theme::TEXT_SECONDARY
    };

    painter.text(
        Pos2::new(rect.min.x + 25.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.5),
        text_color,
    );

    response.on_hover_text(tooltip)
}

fn labeled_slider(
    ui: &mut egui::Ui,
    label: &str,
    tooltip: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    unsigned: bool,
) -> (bool, bool) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(12.5)
                .color(theme::TEXT_SECONDARY),
        )
        .on_hover_text(tooltip);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let formatted = if unsigned {
                format!("{:.0}{suffix}", value.round())
            } else {
                let r = value.round() as i32;
                if r > 0 {
                    format!("+{r}{suffix}")
                } else {
                    format!("{r}{suffix}")
                }
            };
            ui.label(
                RichText::new(formatted)
                    .monospace()
                    .size(12.0)
                    .color(theme::TEXT_PRIMARY),
            )
            .on_hover_text(tooltip);
        });
    });
    let resp = ui
        .add_sized(
            [ui.available_width(), 20.0],
            Slider::new(value, range).step_by(1.0).show_value(false),
        )
        .on_hover_text(tooltip);
    ui.add_space(3.0);
    (resp.changed(), resp.drag_stopped())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_dir_appends_recortadas_suffix() {
        assert_eq!(
            default_output_dir(Path::new("/mnt/datos/escaneos")),
            PathBuf::from("/mnt/datos/escaneos_recortadas")
        );
        assert_eq!(
            default_output_dir(Path::new("album")),
            PathBuf::from("album_recortadas")
        );
    }

    #[test]
    fn export_path_preserves_subfolders_and_extension() {
        let p = build_export_path(
            Path::new("/out"),
            "Escaneo/01/img001.jpg",
            0,
            ExportFormat::Png,
        );
        assert_eq!(p, PathBuf::from("/out/Escaneo/01/img001_foto01.png"));

        let p2 = build_export_path(
            Path::new("/out"),
            "img004.jpg",
            2,
            ExportFormat::Jpeg { quality: 96 },
        );
        assert_eq!(p2, PathBuf::from("/out/img004_foto03.jpg"));
    }

    #[test]
    fn truncate_path_keeps_tail_readable() {
        let short = Path::new("/a/b");
        assert_eq!(truncate_path(short, 20), "/a/b");
        let long = Path::new("/mnt/datos/usuarios/albumes/vacaciones_1998/Escaneo_01");
        let t = truncate_path(long, 24);
        assert!(t.starts_with("..."));
        assert!(t.ends_with("Escaneo_01"));
    }
}
