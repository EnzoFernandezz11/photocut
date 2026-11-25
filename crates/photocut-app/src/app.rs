//! Aplicación principal de escritorio (`eframe::App`) con estética darkroom.
//!
//! Higiene de texturas estricta: mantiene como máximo 2 `TextureHandle` en GPU
//! (`preview_tex` y `backdrop_tex`), sobreescribiéndolos in-place sin fugas de VRAM.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Align, Button, Color32, ComboBox, CornerRadius, Frame, Key, Layout, Margin, Pos2, Rect,
    RichText, ScrollArea, Sense, Slider, SliderClamping, StrokeKind, TextureHandle,
    TextureOptions, Vec2,
};
use image::RgbImage;
use photocut_core::session::{ScanEntry, Session};
use photocut_core::{Adjustments, DetectedPhoto, ExportFormat, OrientedRect, QuarterTurn, ReviewStatus};

use crate::crop_editor::{CropEditorState, MIN_CROP_SIZE, show_crop_editor};
use crate::theme;
use crate::worker::{WorkerCmd, WorkerEvent, WorkerHandle, full_output_dims};

/// Argumentos opcionales de línea de comandos para abrir una carpeta directamente.
#[derive(Debug, Default, Clone)]
pub struct CliArgs {
    pub input_dir: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
}

/// Deriva `<input_dir>_recortadas` como carpeta de salida por defecto.
pub fn default_output_dir(input_dir: &Path) -> PathBuf {
    let cleaned = input_dir.to_path_buf();
    let name = cleaned
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("fotos");
    let out_name = format!("{name}_recortadas");
    match cleaned.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(out_name),
        _ => PathBuf::from(out_name),
    }
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

    // Estado de la carpeta / sesión actual
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

    // Zoom y paneo en el visor normal
    zoom: f32,
    pan: Vec2,

    // Texturas GPU (estrictamente acotadas a 2 slots: sin fugas de memoria/VRAM)
    preview_tex: Option<TextureHandle>,
    preview_meta: Option<PreviewMeta>,
    backdrop_tex: Option<TextureHandle>,
    backdrop_meta: Option<BackdropMeta>,

    // Seguimiento de solicitudes en vuelo
    next_preview_seq: u64,
    exports_in_flight: usize,
    pending_new_crop_recenter: Option<(usize, usize)>,
    status_banner: Option<StatusBanner>,
}

impl PhotoCutApp {
    pub fn new(ctx: egui::Context, cli: CliArgs) -> Self {
        let worker = WorkerHandle::spawn(Some(ctx));
        let mut app = Self {
            worker,
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
            ai_status_detail: "Cargando modelo...".to_string(),
            original_crops: HashMap::new(),
            scan_dims: HashMap::new(),
            selected_scan: 0,
            selected_photo_pos: 0,
            recrop_mode: false,
            crop_editor: CropEditorState::default(),
            show_original_held: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            preview_tex: None,
            preview_meta: None,
            backdrop_tex: None,
            backdrop_meta: None,
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
            app.output_is_auto = is_auto;
            app.open_folder(input_dir, output_dir);
        }

        app
    }

    fn open_folder(&mut self, input_dir: PathBuf, output_dir: PathBuf) {
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
        self.pending_new_crop_recenter = None;
        self.status_banner = Some(StatusBanner {
            kind: StatusKind::Info,
            text: format!("Abriendo carpeta {}...", input_dir.display()),
        });

        self.worker.send(WorkerCmd::OpenFolder {
            input_dir,
            output_dir,
            use_ai_orient: self.ai_orient_enabled,
        });
    }

    fn pick_input_folder(&mut self) {
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
            self.open_folder(in_dir, out_dir);
        }
    }

    fn pick_output_folder(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Seleccionar carpeta de salida");
        if let Some(dir) = self.output_dir.as_ref().or(self.input_dir.as_ref()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(out_dir) = dialog.pick_folder() {
            self.output_is_auto = false;
            self.output_dir = Some(out_dir.clone());
            if let Some(in_dir) = self.input_dir.clone() {
                self.open_folder(in_dir, out_dir);
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
        {
            return;
        }
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
            self.crop_editor.clear_drag();
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
            // Si estamos en la última, saltar a la primera pendiente si existe.
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
        self.crop_editor.clear_drag();
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

        let output_path = build_export_path(&output_dir, &rel_path, snapshot.id, self.export_format);
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
        self.crop_editor.clear_drag();
        self.save_session();
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

                    if self.total_scans == 0 {
                        self.status_banner = Some(StatusBanner {
                            kind: StatusKind::Error,
                            text: "No se encontraron imágenes (JPG/PNG/TIFF) en la carpeta seleccionada."
                                .to_string(),
                        });
                    } else {
                        self.status_banner = None;
                        // Seleccionar la primera foto pendiente disponible (o la primera foto existente).
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

                    if !had_selection && let Some((si, pi)) = self.find_initial_selection() {
                        self.selected_scan = si;
                        self.selected_photo_pos = pi;
                        self.request_current_preview();
                    }
                }
                WorkerEvent::ScanningFinished { total_scans } => {
                    self.total_scans = total_scans;
                    self.scanning_active = false;
                }
                WorkerEvent::PreviewReady {
                    seq,
                    scan_idx,
                    photo_id,
                    image,
                    full_dims,
                    show_original,
                } => {
                    // Ignorar previews desfasados de otra foto o secuencia anterior.
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
        if self.input_dir.is_none() || ctx.wants_keyboard_input() {
            return;
        }

        // Mantener Espacio para Antes / Después en modo revisión.
        let space_down = !self.recrop_mode && ctx.input(|i| i.key_down(Key::Space));
        if space_down != self.show_original_held {
            self.show_original_held = space_down;
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
            self.reset_view();
        }
    }

    fn photo_counts(&self) -> (usize, usize) {
        let mut approved = 0usize;
        let mut total = 0usize;
        for scan in &self.scans {
            for p in &scan.photos {
                total += 1;
                if p.status == ReviewStatus::Approved {
                    approved += 1;
                }
            }
        }
        (approved, total)
    }

    fn show_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar")
            .exact_height(46.0)
            .frame(
                Frame::new()
                    .fill(theme::BG_SURFACE)
                    .inner_margin(Margin::symmetric(12, 6))
                    .stroke(theme::stroke(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new("PhotoCut")
                            .strong()
                            .size(15.5)
                            .color(theme::TEXT_PRIMARY),
                    );
                    ui.separator();

                    if ui
                        .add(Button::new("Carpeta entrada").min_size(Vec2::new(0.0, 28.0)))
                        .on_hover_text(
                            self.input_dir
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "Elegir carpeta con escaneos".to_string()),
                        )
                        .clicked()
                    {
                        self.pick_input_folder();
                    }

                    if ui
                        .add(Button::new("Carpeta salida").min_size(Vec2::new(0.0, 28.0)))
                        .on_hover_text(
                            self.output_dir
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| {
                                    "Elegir carpeta donde guardar las fotos recortadas".to_string()
                                }),
                        )
                        .clicked()
                    {
                        self.pick_output_folder();
                    }

                    // Píldora de progreso
                    if self.input_dir.is_some() {
                        ui.separator();
                        let (approved, total) = self.photo_counts();
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
                            });

                        if self.scanning_active {
                            let indexed = self.indexed_flags.iter().filter(|&&f| f).count();
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(
                                RichText::new(format!("Escaneando {indexed}/{}", self.total_scans))
                                    .size(12.0)
                                    .color(theme::TEXT_SECONDARY),
                            );
                        } else if self.exports_in_flight > 0 {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(
                                RichText::new("Guardando 600 DPI...")
                                    .size(12.0)
                                    .color(theme::ACCENT_HOVER),
                            );
                        }
                    }

                    // Controles alineados a la derecha: Formato de exportación + IA Orientación
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // Insignia de IA Orientación
                        let (ai_label, ai_fill, ai_text) = if self.ai_model_loading {
                            (
                                "IA Orientación: Cargando...",
                                theme::BG_CARD,
                                theme::TEXT_SECONDARY,
                            )
                        } else if !self.ai_orient_available {
                            (
                                "IA Orientación: No disponible",
                                theme::BG_CARD,
                                theme::TEXT_SECONDARY,
                            )
                        } else if self.ai_orient_enabled {
                            (
                                "IA Orientación: ON",
                                Color32::from_rgba_unmultiplied(59, 130, 246, 36),
                                theme::ACCENT_HOVER,
                            )
                        } else {
                            (
                                "IA Orientación: OFF",
                                theme::BG_CARD,
                                theme::TEXT_SECONDARY,
                            )
                        };

                        let ai_btn = Button::new(RichText::new(ai_label).size(12.0).color(ai_text))
                            .fill(ai_fill)
                            .stroke(theme::stroke(
                                1.0,
                                if self.ai_orient_available && self.ai_orient_enabled {
                                    theme::ACCENT
                                } else {
                                    theme::BORDER
                                },
                            ))
                            .corner_radius(CornerRadius::same(12))
                            .min_size(Vec2::new(0.0, 26.0));

                        let resp = ui
                            .add_enabled(self.ai_orient_available, ai_btn)
                            .on_hover_text(&self.ai_status_detail)
                            .on_disabled_hover_text(&self.ai_status_detail);
                        if resp.clicked() {
                            self.ai_orient_enabled = !self.ai_orient_enabled;
                        }

                        ui.separator();

                        // Slider de calidad JPEG inline solo cuando JPEG está activo
                        if let ExportFormat::Jpeg { quality } = &mut self.export_format {
                            let mut q = *quality;
                            let changed = ui
                                .add(
                                    Slider::new(&mut q, 80..=100)
                                        .prefix("Calidad ")
                                        .suffix("%")
                                        .integer(),
                                )
                                .changed();
                            if changed {
                                *quality = q;
                                self.last_jpeg_quality = q;
                                self.save_session();
                            }
                        }

                        // Selector de formato de exportación
                        let fmt_label = match self.export_format {
                            ExportFormat::Png => "PNG (Sin pérdida)",
                            ExportFormat::Tiff => "TIFF (Sin pérdida)",
                            ExportFormat::Jpeg { .. } => "JPEG (Alta calidad)",
                        };
                        let mut new_format = self.export_format;
                        ComboBox::from_id_salt("export_format_combo")
                            .selected_text(fmt_label)
                            .width(168.0)
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
                            });
                        if new_format != self.export_format {
                            self.export_format = new_format;
                            self.save_session();
                        }
                    });
                });
            });
    }

    fn show_empty_state(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG_VIEWPORT))
            .show(ctx, |ui| {
                ui.centered_and_justified(|ui| {
                    Frame::new()
                        .fill(theme::BG_SURFACE)
                        .stroke(theme::stroke(1.0, theme::BORDER))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(32))
                        .show(ui, |ui| {
                            ui.set_max_width(500.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new("PhotoCut")
                                        .size(22.0)
                                        .strong()
                                        .color(theme::TEXT_PRIMARY),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(
                                        "Recorte automático y restauración no destructiva de fotos escaneadas a máxima resolución.",
                                    )
                                    .size(13.5)
                                    .color(theme::TEXT_SECONDARY),
                                );
                                ui.add_space(22.0);

                                workflow_step(
                                    ui,
                                    "1",
                                    "Abrí tu carpeta de escaneos",
                                    "Detecta automáticamente cada foto, endereza la inclinación y sugiere la orientación correcta.",
                                );
                                ui.add_space(12.0);
                                workflow_step(
                                    ui,
                                    "2",
                                    "Revisá y ajustá sin pérdida",
                                    "Rotá con Q / E, re-recortá sobre el escaneo original con C y compará Antes / Después con Espacio.",
                                );
                                ui.add_space(12.0);
                                workflow_step(
                                    ui,
                                    "3",
                                    "Aprobá y guardá a 600 DPI",
                                    "Pulsá Enter para exportar con remuestreo bicúbico y avanzar automáticamente a la siguiente foto.",
                                );

                                ui.add_space(26.0);

                                let open_btn = Button::new(
                                    RichText::new("Abrir carpeta de escaneos")
                                        .size(14.5)
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(theme::ACCENT)
                                .corner_radius(CornerRadius::same(6))
                                .min_size(Vec2::new(ui.available_width(), 40.0));

                                if ui.add(open_btn).clicked() {
                                    self.pick_input_folder();
                                }

                                ui.add_space(10.0);
                                ui.label(
                                    RichText::new(
                                        "Las fotos recortadas se guardarán por defecto en <carpeta>_recortadas (editable desde la barra superior).",
                                    )
                                    .size(11.5)
                                    .color(theme::TEXT_SECONDARY),
                                );
                            });
                        });
                });
            });
    }

    fn show_left_filmstrip(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("filmstrip_panel")
            .default_width(225.0)
            .width_range(190.0..=300.0)
            .frame(
                Frame::new()
                    .fill(theme::BG_SURFACE)
                    .inner_margin(Margin::same(10))
                    .stroke(theme::stroke(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                // Encabezado del panel
                let folder_name = self
                    .input_dir
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("Escaneos");
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("ESCANEOS Y FOTOS")
                            .size(11.0)
                            .strong()
                            .color(theme::TEXT_SECONDARY),
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

                // Botón fijo al pie: + Agregar recorte
                egui::TopBottomPanel::bottom("filmstrip_footer")
                    .frame(Frame::new().inner_margin(Margin { top: 8, ..Margin::ZERO }))
                    .show_inside(ui, |ui| {
                        let can_add = !self.scans.is_empty();
                        let add_btn = Button::new(
                            RichText::new("+ Agregar recorte")
                                .size(12.5)
                                .color(theme::TEXT_PRIMARY),
                        )
                        .fill(theme::BG_CARD)
                        .stroke(theme::stroke(1.0, theme::BORDER))
                        .min_size(Vec2::new(ui.available_width(), 32.0));
                        if ui
                            .add_enabled(can_add, add_btn)
                            .on_hover_text(
                                "Añade manualmente un recorte nuevo sobre el escaneo actual",
                            )
                            .clicked()
                        {
                            self.add_crop_to_current_scan();
                        }
                    });

                // Lista desplazable de escaneos y fotos
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
                                let row_resp = filmstrip_row(
                                    ui,
                                    &format!(
                                        "{stem} · {}",
                                        if is_indexed {
                                            "Sin fotos"
                                        } else {
                                            "Pendiente..."
                                        }
                                    ),
                                    theme::TEXT_SECONDARY,
                                    is_sel_scan,
                                    true,
                                );
                                if row_resp.clicked() && is_indexed {
                                    clicked_empty_scan = Some(si);
                                }
                                continue;
                            }

                            for (pi, photo) in scan.photos.iter().enumerate() {
                                let selected =
                                    self.selected_scan == si && self.selected_photo_pos == pi;
                                let dot_color = match photo.status {
                                    ReviewStatus::Pending => theme::TEXT_SECONDARY,
                                    ReviewStatus::Approved => theme::APPROVED,
                                    ReviewStatus::Skipped => theme::SKIPPED,
                                };
                                let label = format!("{stem} · Foto {}", photo.id + 1);
                                let muted = photo.status == ReviewStatus::Skipped;
                                let resp = filmstrip_row(ui, &label, dot_color, selected, muted);
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
            .default_width(292.0)
            .width_range(268.0..=340.0)
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
                            .inner_margin(Margin { top: 10, ..Margin::ZERO }),
                    )
                    .show_inside(ui, |ui| {
                        ui.separator();
                        ui.add_space(8.0);

                        let ok_btn = Button::new(
                            RichText::new("OK y Guardar  [Enter]")
                                .size(13.5)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(theme::ACCENT)
                        .corner_radius(CornerRadius::same(6))
                        .min_size(Vec2::new(ui.available_width(), 38.0));

                        if ui.add_enabled(has_photo, ok_btn).clicked() {
                            self.approve_and_export_current();
                        }

                        ui.add_space(6.0);

                        let skip_btn = Button::new(
                            RichText::new("Omitir  [Del]")
                                .size(12.5)
                                .color(theme::TEXT_SECONDARY),
                        )
                        .fill(theme::BG_CARD)
                        .stroke(theme::stroke(1.0, theme::BORDER))
                        .min_size(Vec2::new(ui.available_width(), 30.0));

                        if ui.add_enabled(has_photo, skip_btn).clicked() {
                            self.skip_current();
                        }

                        ui.add_space(6.0);

                        let avail = ui.available_width();
                        let half = (avail - 6.0) * 0.5;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if ui
                                .add_enabled(
                                    has_photo,
                                    Button::new("← Anterior").min_size(Vec2::new(half, 30.0)),
                                )
                                .clicked()
                            {
                                self.select_prev();
                            }
                            if ui
                                .add_enabled(
                                    has_photo,
                                    Button::new("Siguiente →").min_size(Vec2::new(half, 30.0)),
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
                        section_heading(ui, "POSICIÓN Y RECORTE");
                        ui.add_space(6.0);

                        let avail = ui.available_width();
                        let half = (avail - 6.0) * 0.5;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if ui
                                .add(Button::new("90° Izq  [Q]").min_size(Vec2::new(half, 30.0)))
                                .clicked()
                            {
                                photo.turn = photo.turn.rotate_ccw();
                                preview_dirty = true;
                                save_needed = true;
                            }
                            if ui
                                .add(Button::new("90° Der  [E]").min_size(Vec2::new(half, 30.0)))
                                .clicked()
                            {
                                photo.turn = photo.turn.rotate_cw();
                                preview_dirty = true;
                                save_needed = true;
                            }
                        });

                        ui.add_space(8.0);

                        // Ángulo fino (-15°..+15°) + reset 0°
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Ángulo fino")
                                    .size(12.5)
                                    .color(theme::TEXT_SECONDARY),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        Button::new(RichText::new("0°").size(11.5))
                                            .min_size(Vec2::new(28.0, 20.0)),
                                    )
                                    .on_hover_text("Restablecer ángulo a 0°")
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

                        let angle_resp = ui.add_sized(
                            [ui.available_width(), 20.0],
                            Slider::new(&mut photo.crop.angle_deg, -15.0..=15.0)
                                .clamping(SliderClamping::Never)
                                .step_by(0.1)
                                .show_value(false),
                        );
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
                        let recrop_fill = if self.recrop_mode {
                            theme::RECROP
                        } else {
                            Color32::from_rgba_unmultiplied(245, 158, 11, 28)
                        };
                        let recrop_text = if self.recrop_mode {
                            theme::BG_VIEWPORT
                        } else {
                            theme::RECROP
                        };
                        let recrop_btn = Button::new(
                            RichText::new(recrop_label)
                                .size(13.0)
                                .strong()
                                .color(recrop_text),
                        )
                        .fill(recrop_fill)
                        .stroke(theme::stroke(1.2, theme::RECROP))
                        .min_size(Vec2::new(ui.available_width(), 34.0));

                        let toggle_recrop = ui.add(recrop_btn).clicked();

                        ui.add_space(16.0);
                        ui.separator();
                        ui.add_space(12.0);

                        // === SECCIÓN 2: COLOR Y LUMINOSIDAD ===
                        section_heading(ui, "COLOR Y LUMINOSIDAD");
                        ui.add_space(8.0);

                        let adj = &mut photo.adjustments;
                        let mut auto_pct = adj.auto_color_strength * 100.0;
                        let (c, s) = labeled_slider(ui, "Auto-Color", &mut auto_pct, 0.0..=100.0, "%", true);
                        if c {
                            adj.auto_color_strength = (auto_pct / 100.0).clamp(0.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut bri = adj.brightness * 100.0;
                        let (c, s) = labeled_slider(ui, "Brillo", &mut bri, -100.0..=100.0, "", false);
                        if c {
                            adj.brightness = (bri / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut con = adj.contrast * 100.0;
                        let (c, s) = labeled_slider(ui, "Contraste", &mut con, -100.0..=100.0, "", false);
                        if c {
                            adj.contrast = (con / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut sat = adj.saturation * 100.0;
                        let (c, s) = labeled_slider(ui, "Saturación", &mut sat, -100.0..=100.0, "", false);
                        if c {
                            adj.saturation = (sat / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut temp = adj.temperature * 100.0;
                        let (c, s) = labeled_slider(ui, "Temperatura", &mut temp, -100.0..=100.0, "", false);
                        if c {
                            adj.temperature = (temp / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        let mut tint = adj.tint * 100.0;
                        let (c, s) = labeled_slider(ui, "Tinte", &mut tint, -100.0..=100.0, "", false);
                        if c {
                            adj.tint = (tint / 100.0).clamp(-1.0, 1.0);
                            preview_dirty = true;
                        }
                        save_needed |= s;

                        ui.add_space(8.0);
                        let reset_btn = Button::new(
                            RichText::new("Restablecer color")
                                .size(12.5)
                                .color(theme::TEXT_SECONDARY),
                        )
                        .fill(theme::BG_CARD)
                        .stroke(theme::stroke(1.0, theme::BORDER))
                        .min_size(Vec2::new(ui.available_width(), 28.0));

                        if ui.add(reset_btn).clicked() {
                            photo.adjustments = Adjustments::default();
                            preview_dirty = true;
                            save_needed = true;
                        }

                        // Persistir mutaciones sobre la foto seleccionada
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
                // Banner sutil de estado / error si existe
                if let Some(banner) = &self.status_banner {
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
                        .inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&banner.text).size(12.5).color(fg));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui
                                        .add(Button::new("×").frame(false))
                                        .on_hover_text("Cerrar aviso")
                                        .clicked()
                                    {
                                        dismiss = true;
                                    }
                                });
                            });
                        });
                    if dismiss {
                        self.status_banner = None;
                    }
                }

                if self.recrop_mode {
                    self.show_recrop_viewport(ui);
                } else {
                    self.show_review_viewport(ui);
                }
            });
    }

    fn show_recrop_viewport(&mut self, ui: &mut egui::Ui) {
        // Barra superior del modo Re-recortar
        let mut margin_delta: Option<f32> = None;
        let mut restore_original = false;
        let mut finish_recrop = false;

        Frame::new()
            .fill(theme::BG_SURFACE)
            .stroke(theme::stroke(1.0, theme::BORDER))
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(245, 158, 11, 30))
                        .stroke(theme::stroke(1.0, theme::RECROP))
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("RE-RECORTAR")
                                    .size(11.5)
                                    .strong()
                                    .color(theme::RECROP),
                            );
                        });

                    ui.add_space(6.0);

                    if ui.button("+20 px margen").clicked() {
                        margin_delta = Some(20.0);
                    }
                    if ui.button("-20 px margen").clicked() {
                        margin_delta = Some(-20.0);
                    }
                    if ui.button("Restaurar recorte detectado").clicked() {
                        restore_original = true;
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let done_btn = Button::new(
                            RichText::new("Listo  [C]")
                                .strong()
                                .color(theme::BG_VIEWPORT),
                        )
                        .fill(theme::RECROP)
                        .min_size(Vec2::new(88.0, 26.0));
                        if ui.add(done_btn).clicked() {
                            finish_recrop = true;
                        }
                    });
                });
            });

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

        // Pie de dimensiones del recorte
        let footer_text = self.current_photo().map(|p| {
            format!(
                "Recorte original: {} × {} px · Ángulo: {:+.1}° · Arrastrá esquinas, bordes, interior o el punto superior para rotar",
                p.crop.width.round() as u32,
                p.crop.height.round() as u32,
                p.crop.angle_deg
            )
        });

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
                });
            });

        // Canvas interactivo sobre el escaneo completo
        let backdrop_ready = self
            .backdrop_meta
            .as_ref()
            .filter(|m| m.scan_idx == self.selected_scan)
            .map(|m| m.orig_dims);

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
        // Pie de resolución y controles de zoom
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
            .exact_height(32.0)
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
                        if ui
                            .add(Button::new("Ajustar [F]").min_size(Vec2::new(0.0, 22.0)))
                            .clicked()
                        {
                            self.reset_view();
                        }
                        ui.label(
                            RichText::new(format!("{:.0}%", self.zoom * 100.0))
                                .monospace()
                                .size(12.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                        ui.separator();
                        ui.label(
                            RichText::new("Mantener [Espacio] = Antes / Después")
                                .size(11.5)
                                .color(theme::TEXT_SECONDARY),
                        );
                    });
                });
            });

        // Área principal de previsualización
        let available = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(available, Sense::click_and_drag());

        // Zoom con rueda del mouse y paneo con arrastre
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
            let disp_size = Vec2::new(img_w * base_scale * self.zoom, img_h * base_scale * self.zoom);
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

            // Insignia flotante Antes / Después al mantener Espacio
            if self.show_original_held || meta.show_original {
                let badge_pos = available.min + Vec2::new(16.0, 16.0);
                let label = "ANTES (Original)";
                let galley = painter.layout_no_wrap(
                    label.to_string(),
                    egui::FontId::proportional(12.5),
                    theme::BG_VIEWPORT,
                );
                let badge_rect = Rect::from_min_size(
                    badge_pos,
                    galley.size() + Vec2::new(20.0, 10.0),
                );
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
                "No hay fotos detectadas en este escaneo. Usá \"+ Agregar recorte\" para crear una."
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
        self.show_top_bar(ctx);

        if self.input_dir.is_none() {
            self.show_empty_state(ctx);
        } else {
            self.show_left_filmstrip(ctx);
            self.show_right_inspector(ctx);
            self.show_center_viewport(ctx);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.worker.shutdown();
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
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
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
) -> egui::Response {
    let desired = Vec2::new(ui.available_width(), 28.0);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());

    let bg = if selected {
        theme::BG_CARD
    } else if response.hovered() {
        theme::BG_CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };

    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(5), bg);

    if selected {
        let bar = Rect::from_min_max(
            Pos2::new(rect.min.x, rect.min.y + 4.0),
            Pos2::new(rect.min.x + 3.0, rect.max.y - 4.0),
        );
        painter.rect_filled(bar, CornerRadius::same(2), theme::ACCENT);
    }

    let dot_center = Pos2::new(rect.min.x + 14.0, rect.center().y);
    painter.circle_filled(dot_center, 4.0, dot_color);

    let text_color = if selected {
        theme::TEXT_PRIMARY
    } else if muted {
        theme::SKIPPED
    } else {
        theme::TEXT_SECONDARY
    };

    painter.text(
        Pos2::new(rect.min.x + 26.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.5),
        text_color,
    );

    response
}

fn labeled_slider(
    ui: &mut egui::Ui,
    label: &str,
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
        );
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
            );
        });
    });
    let resp = ui.add_sized(
        [ui.available_width(), 20.0],
        Slider::new(value, range).step_by(1.0).show_value(false),
    );
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
}
