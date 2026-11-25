//! Editor interactivo de Re-recortar directamente sobre el escaneo original.
//!
//! Muestra la hoja completa con máscara gris translúcida fuera del recorte y un
//! rectángulo verde brillante (`#22C55E`) con 4 esquinas grandes fáciles de agarrar,
//! 4 bordes completos arrastrables, rotación superior y zoom/paneo suaves.

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, PointerButton, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, TextureHandle, Vec2,
};
use photocut_core::OrientedRect;

use crate::theme;

/// Tamaño mínimo permitido en píxeles del escaneo original para evitar colapsos.
pub const MIN_CROP_SIZE: f32 = 32.0;

const CORNER_HIT_RADIUS: f32 = 22.0;
const EDGE_HIT_RADIUS: f32 = 12.0;
const ROTATE_HIT_RADIUS: f32 = 18.0;
const ROTATE_STEM_SCREEN_PX: f32 = 30.0;
const VIEWPORT_PADDING_PX: f32 = 36.0;
const MIN_ZOOM: f32 = 1.0;
const MAX_ZOOM: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CornerHandle {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

impl CornerHandle {
    pub const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomRight,
        Self::BottomLeft,
    ];

    /// Signos `(sx, sy)` en el marco local del rectángulo orientado.
    fn signs(self) -> (f32, f32) {
        match self {
            Self::TopLeft => (-1.0, -1.0),
            Self::TopRight => (1.0, -1.0),
            Self::BottomRight => (1.0, 1.0),
            Self::BottomLeft => (-1.0, 1.0),
        }
    }

    fn corner_index(self) -> usize {
        match self {
            Self::TopLeft => 0,
            Self::TopRight => 1,
            Self::BottomRight => 2,
            Self::BottomLeft => 3,
        }
    }

    fn cursor(self) -> CursorIcon {
        match self {
            Self::TopLeft | Self::BottomRight => CursorIcon::ResizeNwSe,
            Self::TopRight | Self::BottomLeft => CursorIcon::ResizeNeSw,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeHandle {
    Top,
    Right,
    Bottom,
    Left,
}

impl EdgeHandle {
    pub const ALL: [Self; 4] = [Self::Top, Self::Right, Self::Bottom, Self::Left];

    /// Índices de las dos esquinas que definen el segmento completo de este borde.
    fn corner_pair(self) -> (usize, usize) {
        match self {
            Self::Top => (0, 1),
            Self::Right => (1, 2),
            Self::Bottom => (2, 3),
            Self::Left => (3, 0),
        }
    }

    fn cursor(self) -> CursorIcon {
        match self {
            Self::Top | Self::Bottom => CursorIcon::ResizeVertical,
            Self::Left | Self::Right => CursorIcon::ResizeHorizontal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragHandle {
    Corner(CornerHandle),
    Edge(EdgeHandle),
    Body,
    Rotate,
}

#[derive(Debug, Clone, Copy)]
struct ActiveCropDrag {
    handle: DragHandle,
    start_crop: OrientedRect,
    start_scan_pos: (f32, f32),
}

#[derive(Debug, Clone, Copy)]
enum DragAction {
    Crop(ActiveCropDrag),
    Pan {
        start_pan: Vec2,
        start_screen_pos: Pos2,
    },
}

/// Estado persistente de interacción y cámara (zoom/paneo) del editor de recorte.
#[derive(Debug)]
pub struct CropEditorState {
    active_drag: Option<DragAction>,
    pub zoom: f32,
    pub pan: Vec2,
    last_viewport: Option<Rect>,
}

impl Default for CropEditorState {
    fn default() -> Self {
        Self {
            active_drag: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            last_viewport: None,
        }
    }
}

impl CropEditorState {
    pub fn clear_drag(&mut self) {
        self.active_drag = None;
    }

    /// Muestra la hoja escaneada completa centrada (`zoom = 1.0`, `pan = 0`).
    pub fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.active_drag = None;
    }

    /// Hace zoom cómodo sobre la foto activa (~65% del visor) dejando margen gris alrededor.
    pub fn focus_on_crop(
        &mut self,
        viewport: Rect,
        orig_scan_dims: (u32, u32),
        crop: OrientedRect,
    ) {
        let base = ScanViewportTransform::fit(viewport, orig_scan_dims);
        let crop_screen_w = (crop.width * base.scale).max(1.0);
        let crop_screen_h = (crop.height * base.scale).max(1.0);
        let target_zoom = ((viewport.width() * 0.65) / crop_screen_w)
            .min((viewport.height() * 0.65) / crop_screen_h)
            .clamp(MIN_ZOOM, 3.5);

        let new_scale = base.scale * target_zoom;
        let sw = (orig_scan_dims.0 as f32).max(1.0);
        let sh = (orig_scan_dims.1 as f32).max(1.0);

        self.zoom = target_zoom;
        self.pan = Vec2::new(
            (sw * 0.5 - crop.cx) * new_scale,
            (sh * 0.5 - crop.cy) * new_scale,
        );
        self.clamp_pan(viewport, orig_scan_dims);
        self.active_drag = None;
    }

    pub fn last_viewport(&self) -> Option<Rect> {
        self.last_viewport
    }

    fn clamp_pan(&mut self, viewport: Rect, orig_scan_dims: (u32, u32)) {
        if self.zoom <= 1.001 {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
            return;
        }
        let base = ScanViewportTransform::fit(viewport, orig_scan_dims);
        let scaled_w = (orig_scan_dims.0 as f32).max(1.0) * base.scale * self.zoom;
        let scaled_h = (orig_scan_dims.1 as f32).max(1.0) * base.scale * self.zoom;
        let max_x = ((scaled_w + viewport.width()) * 0.5 - 80.0).max(0.0);
        let max_y = ((scaled_h + viewport.height()) * 0.5 - 80.0).max(0.0);
        self.pan.x = self.pan.x.clamp(-max_x, max_x);
        self.pan.y = self.pan.y.clamp(-max_y, max_y);
    }
}

/// Transformación afín uniforme entre píxeles del escaneo original y coordenadas de pantalla.
#[derive(Debug, Clone, Copy)]
pub struct ScanViewportTransform {
    pub screen_rect: Rect,
    pub scale: f32,
}

impl ScanViewportTransform {
    pub fn fit(viewport: Rect, scan_dims: (u32, u32)) -> Self {
        Self::from_view(viewport, scan_dims, 1.0, Vec2::ZERO)
    }

    pub fn from_view(viewport: Rect, scan_dims: (u32, u32), zoom: f32, pan: Vec2) -> Self {
        let sw = (scan_dims.0 as f32).max(1.0);
        let sh = (scan_dims.1 as f32).max(1.0);
        let avail_w = (viewport.width() - VIEWPORT_PADDING_PX * 2.0).max(64.0);
        let avail_h = (viewport.height() - VIEWPORT_PADDING_PX * 2.0).max(64.0);
        let base_scale = (avail_w / sw).min(avail_h / sh);
        let scale = base_scale * zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let disp_w = sw * scale;
        let disp_h = sh * scale;
        let screen_rect =
            Rect::from_center_size(viewport.center() + pan, Vec2::new(disp_w, disp_h));
        Self { screen_rect, scale }
    }

    #[inline]
    pub fn scan_to_screen(&self, p: (f32, f32)) -> Pos2 {
        Pos2::new(
            self.screen_rect.min.x + p.0 * self.scale,
            self.screen_rect.min.y + p.1 * self.scale,
        )
    }

    #[inline]
    pub fn screen_to_scan(&self, p: Pos2) -> (f32, f32) {
        (
            (p.x - self.screen_rect.min.x) / self.scale,
            (p.y - self.screen_rect.min.y) / self.scale,
        )
    }
}

/// Resultado de un frame de interacción del editor de recorte.
#[derive(Debug, Default, Clone, Copy)]
pub struct CropEditOutcome {
    /// Verdadero si el rectángulo cambió durante este frame.
    pub changed: bool,
    /// Verdadero si el usuario acaba de soltar el arrastre (ideal para guardar sesión).
    pub drag_finished: bool,
}

/// Renderiza el escaneo completo con la máscara gris oscura y el rectángulo verde interactivo.
pub fn show_crop_editor(
    ui: &mut egui::Ui,
    state: &mut CropEditorState,
    backdrop_tex: &TextureHandle,
    orig_scan_dims: (u32, u32),
    crop: &mut OrientedRect,
    sibling_crops: &[(usize, OrientedRect)],
) -> CropEditOutcome {
    let available = ui.available_rect_before_wrap();
    state.last_viewport = Some(available);
    let response = ui.allocate_rect(available, Sense::click_and_drag());

    // 1. Zoom suave con rueda del mouse centrado bajo el cursor.
    if response.hovered() {
        let (scroll_y, hover_pos, aux_pan_down, pointer_delta) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.pointer.hover_pos(),
                i.pointer.button_down(PointerButton::Middle)
                    || i.pointer.button_down(PointerButton::Secondary),
                i.pointer.delta(),
            )
        });

        if scroll_y.abs() > 0.1 {
            let old_xform =
                ScanViewportTransform::from_view(available, orig_scan_dims, state.zoom, state.pan);
            let cursor = hover_pos.unwrap_or_else(|| available.center());
            let scan_under = old_xform.screen_to_scan(cursor);

            let factor = (1.0 + scroll_y * 0.0022).clamp(0.75, 1.35);
            let new_zoom = (state.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);

            if (new_zoom - MIN_ZOOM).abs() < 0.015 {
                state.zoom = MIN_ZOOM;
                state.pan = Vec2::ZERO;
            } else {
                let base = ScanViewportTransform::fit(available, orig_scan_dims);
                let new_scale = base.scale * new_zoom;
                let sw = (orig_scan_dims.0 as f32).max(1.0);
                let sh = (orig_scan_dims.1 as f32).max(1.0);
                state.zoom = new_zoom;
                state.pan = Vec2::new(
                    cursor.x - available.center().x - (scan_under.0 - sw * 0.5) * new_scale,
                    cursor.y - available.center().y - (scan_under.1 - sh * 0.5) * new_scale,
                );
                state.clamp_pan(available, orig_scan_dims);
            }
        }

        // Paneo con botón central o derecho en cualquier punto del visor.
        if aux_pan_down && pointer_delta != Vec2::ZERO && state.zoom > 1.001 {
            state.pan += pointer_delta;
            state.clamp_pan(available, orig_scan_dims);
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
    }

    let xform = ScanViewportTransform::from_view(available, orig_scan_dims, state.zoom, state.pan);
    let mut outcome = CropEditOutcome::default();

    // 2. Interacción principal (esquinas, bordes completos, interior, rotación o paneo exterior).
    let hovered_handle = response
        .hover_pos()
        .and_then(|pos| hit_test_handles(*crop, &xform, pos));

    if response.drag_started() {
        // Usar `press_origin` (punto exacto donde se hizo clic) antes del umbral de arrastre de egui.
        let press_pos = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos());
        if let Some(press_pos) = press_pos {
            if let Some(handle) = hit_test_handles(*crop, &xform, press_pos) {
                state.active_drag = Some(DragAction::Crop(ActiveCropDrag {
                    handle,
                    start_crop: *crop,
                    start_scan_pos: xform.screen_to_scan(press_pos),
                }));
            } else if state.zoom > 1.001 {
                state.active_drag = Some(DragAction::Pan {
                    start_pan: state.pan,
                    start_screen_pos: press_pos,
                });
            }
        }
    }

    if response.dragged()
        && let Some(action) = state.active_drag
        && let Some(curr_pos) = response.interact_pointer_pos()
    {
        match action {
            DragAction::Crop(drag) => {
                let curr_scan = xform.screen_to_scan(curr_pos);
                let updated = match drag.handle {
                    DragHandle::Corner(c) => apply_corner_drag(drag.start_crop, c, curr_scan),
                    DragHandle::Edge(e) => apply_edge_drag(drag.start_crop, e, curr_scan),
                    DragHandle::Body => apply_body_drag(
                        drag.start_crop,
                        (
                            curr_scan.0 - drag.start_scan_pos.0,
                            curr_scan.1 - drag.start_scan_pos.1,
                        ),
                        orig_scan_dims,
                    ),
                    DragHandle::Rotate => apply_rotation_drag(drag.start_crop, curr_scan),
                };
                if updated != *crop {
                    *crop = updated;
                    outcome.changed = true;
                }
            }
            DragAction::Pan {
                start_pan,
                start_screen_pos,
            } => {
                state.pan = start_pan + (curr_pos - start_screen_pos);
                state.clamp_pan(available, orig_scan_dims);
            }
        }
    }

    if response.drag_stopped() {
        if let Some(DragAction::Crop(_)) = state.active_drag.take() {
            outcome.drag_finished = true;
        }
    }

    // Doble clic fuera del recorte restablece la vista completa.
    if response.double_clicked() && hovered_handle.is_none() {
        state.reset_view();
    }

    // Cursor contextual y tooltip al pasar sobre el punto de rotación.
    let active_crop_handle = match state.active_drag {
        Some(DragAction::Crop(d)) => Some(d.handle),
        _ => None,
    };
    let cursor_handle = active_crop_handle.or(hovered_handle);
    if matches!(state.active_drag, Some(DragAction::Pan { .. })) {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    } else if let Some(h) = cursor_handle {
        let icon = match h {
            DragHandle::Corner(c) => c.cursor(),
            DragHandle::Edge(e) => e.cursor(),
            DragHandle::Body => {
                if active_crop_handle.is_some() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                }
            }
            DragHandle::Rotate => CursorIcon::PointingHand,
        };
        ui.ctx().set_cursor_icon(icon);
    } else if response.hovered() && state.zoom > 1.001 {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }

    if hovered_handle == Some(DragHandle::Rotate) && state.active_drag.is_none() {
        response.clone().on_hover_text("Arrastrá para girar el ángulo fino del recorte");
    }

    // Recalcular transformación si el paneo cambió en este frame.
    let xform = ScanViewportTransform::from_view(available, orig_scan_dims, state.zoom, state.pan);

    // 3. Dibujar fondo del escaneo completo.
    let painter = ui.painter_at(available);
    painter.rect_filled(available, CornerRadius::ZERO, theme::BG_VIEWPORT);

    painter.rect_stroke(
        xform.screen_rect.expand(1.0),
        CornerRadius::ZERO,
        theme::stroke(1.0, theme::BORDER),
        StrokeKind::Outside,
    );

    painter.image(
        backdrop_tex.id(),
        xform.screen_rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );

    // 4. Máscara gris translúcida fuera del rectángulo verde (mantiene legible toda la hoja).
    draw_dimmed_outside_mask(&painter, xform.screen_rect, *crop, &xform);

    // 5. Contornos muy sutiles de otras fotos del mismo escaneo (sin competir con el verde activo).
    for &(other_id, other_rect) in sibling_crops {
        let pts: Vec<Pos2> = other_rect
            .corners()
            .into_iter()
            .map(|c| xform.scan_to_screen(c))
            .collect();
        painter.add(Shape::closed_line(
            pts.clone(),
            theme::stroke(1.0, Color32::from_rgba_unmultiplied(150, 156, 168, 70)),
        ));
        if let Some(&tl) = pts.first() {
            painter.text(
                tl + Vec2::new(6.0, 6.0),
                egui::Align2::LEFT_TOP,
                format!("Foto {}", other_id + 1),
                egui::FontId::proportional(11.0),
                Color32::from_rgba_unmultiplied(236, 238, 242, 95),
            );
        }
    }

    // 6. Guías sutiles de regla de tercios dentro de la foto seleccionada.
    draw_rule_of_thirds(&painter, *crop, &xform);

    // 7. Rectángulo verde brillante y nítido (3.0 px).
    let corners_screen: Vec<Pos2> = crop
        .corners()
        .into_iter()
        .map(|c| xform.scan_to_screen(c))
        .collect();
    painter.add(Shape::closed_line(
        corners_screen,
        theme::stroke(3.0, theme::APPROVED),
    ));

    // 8. Dibujar handles grandes en las 4 esquinas, cápsulas de borde y punto de rotación.
    draw_handles(&painter, *crop, &xform, cursor_handle);

    outcome
}

fn rect_axes(angle_deg: f32) -> ((f32, f32), (f32, f32)) {
    let (sin_a, cos_a) = angle_deg.to_radians().sin_cos();
    ((cos_a, sin_a), (-sin_a, cos_a))
}

fn edge_midpoint_scan(crop: OrientedRect, edge: EdgeHandle) -> (f32, f32) {
    let (u, v) = rect_axes(crop.angle_deg);
    let hw = crop.width * 0.5;
    let hh = crop.height * 0.5;
    match edge {
        EdgeHandle::Top => (crop.cx - hh * v.0, crop.cy - hh * v.1),
        EdgeHandle::Bottom => (crop.cx + hh * v.0, crop.cy + hh * v.1),
        EdgeHandle::Left => (crop.cx - hw * u.0, crop.cy - hw * u.1),
        EdgeHandle::Right => (crop.cx + hw * u.0, crop.cy + hw * u.1),
    }
}

fn rotation_handle_screen(crop: OrientedRect, xform: &ScanViewportTransform) -> (Pos2, Pos2) {
    let (_, v) = rect_axes(crop.angle_deg);
    let top_mid_scan = edge_midpoint_scan(crop, EdgeHandle::Top);
    let top_mid = xform.scan_to_screen(top_mid_scan);
    let knob = Pos2::new(
        top_mid.x - v.0 * ROTATE_STEM_SCREEN_PX,
        top_mid.y - v.1 * ROTATE_STEM_SCREEN_PX,
    );
    (top_mid, knob)
}

fn point_to_segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq < 1e-6 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    let proj = a + ab * t;
    p.distance(proj)
}

pub(crate) fn hit_test_handles(
    crop: OrientedRect,
    xform: &ScanViewportTransform,
    pointer: Pos2,
) -> Option<DragHandle> {
    // 1. Handle superior de rotación.
    let (_, rot_knob) = rotation_handle_screen(crop, xform);
    if pointer.distance(rot_knob) <= ROTATE_HIT_RADIUS {
        return Some(DragHandle::Rotate);
    }

    // 2. Las 4 esquinas (radio generoso de 22 px, con prioridad sobre los bordes).
    let corners_screen = crop.corners().map(|c| xform.scan_to_screen(c));
    let mut best_corner: Option<(CornerHandle, f32)> = None;
    for corner in CornerHandle::ALL {
        let dist = pointer.distance(corners_screen[corner.corner_index()]);
        if dist <= CORNER_HIT_RADIUS
            && best_corner.is_none_or(|(_, best_d)| dist < best_d)
        {
            best_corner = Some((corner, dist));
        }
    }
    if let Some((corner, _)) = best_corner {
        return Some(DragHandle::Corner(corner));
    }

    // 3. Los 4 bordes completos (distancia punto-segmento <= 12 px).
    let mut best_edge: Option<(EdgeHandle, f32)> = None;
    for edge in EdgeHandle::ALL {
        let (i0, i1) = edge.corner_pair();
        let dist = point_to_segment_distance(pointer, corners_screen[i0], corners_screen[i1]);
        if dist <= EDGE_HIT_RADIUS && best_edge.is_none_or(|(_, best_d)| dist < best_d) {
            best_edge = Some((edge, dist));
        }
    }
    if let Some((edge, _)) = best_edge {
        return Some(DragHandle::Edge(edge));
    }

    // 4. Interior del rectángulo orientado (traslación).
    let scan_pt = xform.screen_to_scan(pointer);
    let (u, v) = rect_axes(crop.angle_deg);
    let dx = scan_pt.0 - crop.cx;
    let dy = scan_pt.1 - crop.cy;
    let local_x = dx * u.0 + dy * u.1;
    let local_y = dx * v.0 + dy * v.1;
    if local_x.abs() <= crop.width * 0.5 && local_y.abs() <= crop.height * 0.5 {
        return Some(DragHandle::Body);
    }

    None
}

fn draw_dimmed_outside_mask(
    painter: &egui::Painter,
    clip_rect: Rect,
    crop: OrientedRect,
    xform: &ScanViewportTransform,
) {
    let clipped = painter.with_clip_rect(clip_rect);
    let (u, v) = rect_axes(crop.angle_deg);
    let center = xform.scan_to_screen((crop.cx, crop.cy));
    let hw = crop.width * 0.5 * xform.scale;
    let hh = crop.height * 0.5 * xform.scale;
    let big = (clip_rect.width() + clip_rect.height()) * 2.0 + hw + hh;

    let to_screen = |lx: f32, ly: f32| -> Pos2 {
        Pos2::new(
            center.x + lx * u.0 + ly * v.0,
            center.y + lx * u.1 + ly * v.1,
        )
    };

    let dim = Color32::from_rgba_unmultiplied(18, 20, 24, 178);
    let bands = [
        // Franja superior
        [
            to_screen(-big, -big),
            to_screen(big, -big),
            to_screen(big, -hh),
            to_screen(-big, -hh),
        ],
        // Franja inferior
        [
            to_screen(-big, hh),
            to_screen(big, hh),
            to_screen(big, big),
            to_screen(-big, big),
        ],
        // Franja izquierda entre -hh y +hh
        [
            to_screen(-big, -hh),
            to_screen(-hw, -hh),
            to_screen(-hw, hh),
            to_screen(-big, hh),
        ],
        // Franja derecha entre -hh y +hh
        [
            to_screen(hw, -hh),
            to_screen(big, -hh),
            to_screen(big, hh),
            to_screen(hw, hh),
        ],
    ];

    for quad in bands {
        clipped.add(Shape::convex_polygon(quad.to_vec(), dim, Stroke::NONE));
    }
}

fn draw_rule_of_thirds(
    painter: &egui::Painter,
    crop: OrientedRect,
    xform: &ScanViewportTransform,
) {
    let (u, v) = rect_axes(crop.angle_deg);
    let hw = crop.width * 0.5;
    let hh = crop.height * 0.5;
    let to_screen = |lx: f32, ly: f32| {
        xform.scan_to_screen((
            crop.cx + lx * u.0 + ly * v.0,
            crop.cy + lx * u.1 + ly * v.1,
        ))
    };

    let guide_stroke = theme::stroke(1.0, Color32::from_rgba_unmultiplied(236, 238, 242, 44));
    for frac in [-1.0 / 3.0, 1.0 / 3.0] {
        let lx = hw * frac;
        painter.line_segment([to_screen(lx, -hh), to_screen(lx, hh)], guide_stroke);
        let ly = hh * frac;
        painter.line_segment([to_screen(-hw, ly), to_screen(hw, ly)], guide_stroke);
    }
}

fn draw_handles(
    painter: &egui::Painter,
    crop: OrientedRect,
    xform: &ScanViewportTransform,
    highlighted: Option<DragHandle>,
) {
    let active_green = Color32::from_rgb(0x4A, 0xDE, 0x80);
    let dark_outline = theme::stroke(2.0, theme::BG_VIEWPORT);

    // Handle superior de rotación (verde/blanco con borde oscuro)
    let (top_mid, rot_knob) = rotation_handle_screen(crop, xform);
    let rot_active = highlighted == Some(DragHandle::Rotate);
    painter.line_segment(
        [top_mid, rot_knob],
        theme::stroke(2.0, if rot_active { Color32::WHITE } else { theme::APPROVED }),
    );
    let rot_r = if rot_active { 8.5 } else { 7.0 };
    painter.circle(
        rot_knob,
        rot_r + 1.0,
        if rot_active { active_green } else { theme::APPROVED },
        dark_outline,
    );
    painter.circle_stroke(rot_knob, (rot_r - 1.5).max(2.0), theme::stroke(1.8, Color32::WHITE));

    // 4 cápsulas sutiles en el punto medio de cada borde (indican que los bordes son arrastrables)
    let (u, v) = rect_axes(crop.angle_deg);
    for edge in EdgeHandle::ALL {
        let mid = xform.scan_to_screen(edge_midpoint_scan(crop, edge));
        let active = highlighted == Some(DragHandle::Edge(edge));
        let dir = match edge {
            EdgeHandle::Top | EdgeHandle::Bottom => Vec2::new(u.0, u.1),
            EdgeHandle::Left | EdgeHandle::Right => Vec2::new(v.0, v.1),
        };
        let half_len = if active { 15.0 } else { 12.0 };
        let color = if active { Color32::WHITE } else { theme::APPROVED };
        painter.line_segment(
            [mid - dir * half_len, mid + dir * half_len],
            theme::stroke(6.5, theme::BG_VIEWPORT),
        );
        painter.line_segment(
            [mid - dir * half_len, mid + dir * half_len],
            theme::stroke(3.6, color),
        );
    }

    // 4 círculos grandes en las esquinas (relleno verde brillante, anillo blanco interior, borde oscuro)
    let corners = crop.corners();
    for corner in CornerHandle::ALL {
        let p = xform.scan_to_screen(corners[corner.corner_index()]);
        let active = highlighted == Some(DragHandle::Corner(corner));
        let radius = if active { 10.0 } else { 8.0 };
        let fill = if active { active_green } else { theme::APPROVED };

        painter.circle(p, radius, fill, dark_outline);
        painter.circle_stroke(p, radius - 2.2, theme::stroke(2.0, Color32::WHITE));
    }
}

/// Arrastra una esquina manteniendo fija la esquina opuesta en coordenadas del escaneo.
pub fn apply_corner_drag(
    start: OrientedRect,
    corner: CornerHandle,
    pointer_scan: (f32, f32),
) -> OrientedRect {
    let (u, v) = rect_axes(start.angle_deg);
    let (sx, sy) = corner.signs();
    let hw = start.width * 0.5;
    let hh = start.height * 0.5;

    // Esquina opuesta anclada.
    let anchor_x = start.cx + (-sx * hw) * u.0 + (-sy * hh) * v.0;
    let anchor_y = start.cy + (-sx * hw) * u.1 + (-sy * hh) * v.1;

    let dx = pointer_scan.0 - anchor_x;
    let dy = pointer_scan.1 - anchor_y;
    let proj_u = dx * u.0 + dy * u.1;
    let proj_v = dx * v.0 + dy * v.1;

    let new_w = (sx * proj_u).max(MIN_CROP_SIZE);
    let new_h = (sy * proj_v).max(MIN_CROP_SIZE);

    let new_cx = anchor_x + (sx * new_w * 0.5) * u.0 + (sy * new_h * 0.5) * v.0;
    let new_cy = anchor_y + (sx * new_w * 0.5) * u.1 + (sy * new_h * 0.5) * v.1;

    OrientedRect {
        cx: new_cx,
        cy: new_cy,
        width: new_w,
        height: new_h,
        angle_deg: start.angle_deg,
    }
}

/// Arrastra un borde lateral manteniendo inmóvil el borde opuesto y la otra dimensión.
pub fn apply_edge_drag(
    start: OrientedRect,
    edge: EdgeHandle,
    pointer_scan: (f32, f32),
) -> OrientedRect {
    let (u, v) = rect_axes(start.angle_deg);
    let hw = start.width * 0.5;
    let hh = start.height * 0.5;

    match edge {
        EdgeHandle::Left | EdgeHandle::Right => {
            let s = if edge == EdgeHandle::Right { 1.0 } else { -1.0 };
            let opp_x = start.cx + (-s * hw) * u.0;
            let opp_y = start.cy + (-s * hw) * u.1;
            let proj = (pointer_scan.0 - opp_x) * u.0 + (pointer_scan.1 - opp_y) * u.1;
            let new_w = (s * proj).max(MIN_CROP_SIZE);
            OrientedRect {
                cx: opp_x + (s * new_w * 0.5) * u.0,
                cy: opp_y + (s * new_w * 0.5) * u.1,
                width: new_w,
                height: start.height,
                angle_deg: start.angle_deg,
            }
        }
        EdgeHandle::Top | EdgeHandle::Bottom => {
            let s = if edge == EdgeHandle::Bottom { 1.0 } else { -1.0 };
            let opp_x = start.cx + (-s * hh) * v.0;
            let opp_y = start.cy + (-s * hh) * v.1;
            let proj = (pointer_scan.0 - opp_x) * v.0 + (pointer_scan.1 - opp_y) * v.1;
            let new_h = (s * proj).max(MIN_CROP_SIZE);
            OrientedRect {
                cx: opp_x + (s * new_h * 0.5) * v.0,
                cy: opp_y + (s * new_h * 0.5) * v.1,
                width: start.width,
                height: new_h,
                angle_deg: start.angle_deg,
            }
        }
    }
}

/// Traslada el centro `(cx, cy)` preservando tamaño y ángulo.
pub fn apply_body_drag(
    start: OrientedRect,
    delta_scan: (f32, f32),
    scan_dims: (u32, u32),
) -> OrientedRect {
    let max_x = scan_dims.0 as f32;
    let max_y = scan_dims.1 as f32;
    OrientedRect {
        cx: (start.cx + delta_scan.0).clamp(0.0, max_x),
        cy: (start.cy + delta_scan.1).clamp(0.0, max_y),
        ..start
    }
}

/// Rota suavemente el recorte usando el handle superior.
pub fn apply_rotation_drag(start: OrientedRect, pointer_scan: (f32, f32)) -> OrientedRect {
    let dx = pointer_scan.0 - start.cx;
    let dy = pointer_scan.1 - start.cy;
    if dx * dx + dy * dy < 1e-4 {
        return start;
    }
    let angle = dx.atan2(-dy).to_degrees().clamp(-45.0, 45.0);
    OrientedRect {
        angle_deg: angle,
        ..start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_pt(a: (f32, f32), b: (f32, f32), eps: f32) {
        assert!(
            (a.0 - b.0).abs() <= eps && (a.1 - b.1).abs() <= eps,
            "points differ: {a:?} vs {b:?} (eps={eps})"
        );
    }

    #[test]
    fn corner_drag_keeps_opposite_corner_anchored_even_when_rotated() {
        let start = OrientedRect {
            cx: 1000.0,
            cy: 1200.0,
            width: 400.0,
            height: 300.0,
            angle_deg: 12.5,
        };
        let orig_corners = start.corners();

        // Arrastrar esquina BR hacia afuera -> TL (índice 0) debe permanecer inmóvil.
        let moved = apply_corner_drag(start, CornerHandle::BottomRight, (1320.0, 1490.0));
        let new_corners = moved.corners();
        approx_pt(new_corners[0], orig_corners[0], 1e-3);
        assert!(moved.width > start.width);
        assert!(moved.height > start.height);
        assert_eq!(moved.angle_deg, start.angle_deg);

        // Arrastrar esquina TL -> BR (índice 2) debe permanecer inmóvil.
        let moved_tl = apply_corner_drag(start, CornerHandle::TopLeft, (720.0, 950.0));
        approx_pt(moved_tl.corners()[2], orig_corners[2], 1e-3);
    }

    #[test]
    fn corner_drag_never_collapses_below_minimum_size() {
        let start = OrientedRect {
            cx: 500.0,
            cy: 500.0,
            width: 200.0,
            height: 200.0,
            angle_deg: -5.0,
        };
        let orig_tl = start.corners()[0];

        // Arrastrar BR mucho más allá de TL (inversión).
        let collapsed = apply_corner_drag(start, CornerHandle::BottomRight, (10.0, 10.0));
        assert_eq!(collapsed.width, MIN_CROP_SIZE);
        assert_eq!(collapsed.height, MIN_CROP_SIZE);
        approx_pt(collapsed.corners()[0], orig_tl, 1e-3);
    }

    #[test]
    fn edge_drag_keeps_opposite_edge_fixed_and_clamps_min_size() {
        let start = OrientedRect {
            cx: 800.0,
            cy: 900.0,
            width: 300.0,
            height: 200.0,
            angle_deg: -8.0,
        };
        let orig_left_mid = edge_midpoint_scan(start, EdgeHandle::Left);

        // Expandir borde derecho ("volver atrás si cortó de más").
        let expanded = apply_edge_drag(start, EdgeHandle::Right, (1100.0, 900.0));
        assert!(expanded.width > start.width);
        assert_eq!(expanded.height, start.height);
        approx_pt(
            edge_midpoint_scan(expanded, EdgeHandle::Left),
            orig_left_mid,
            1e-3,
        );

        // Comprimir borde superior más allá del inferior -> se detiene en MIN_CROP_SIZE y borde inferior intacto.
        let orig_bottom_mid = edge_midpoint_scan(start, EdgeHandle::Bottom);
        let shrunk = apply_edge_drag(start, EdgeHandle::Top, (800.0, 1800.0));
        assert_eq!(shrunk.height, MIN_CROP_SIZE);
        assert_eq!(shrunk.width, start.width);
        approx_pt(
            edge_midpoint_scan(shrunk, EdgeHandle::Bottom),
            orig_bottom_mid,
            1e-3,
        );
    }

    #[test]
    fn body_drag_translates_center_preserving_size_and_angle() {
        let start = OrientedRect {
            cx: 400.0,
            cy: 600.0,
            width: 250.0,
            height: 180.0,
            angle_deg: 3.25,
        };
        let moved = apply_body_drag(start, (75.0, -40.0), (2000, 3000));
        assert_eq!(moved.cx, 475.0);
        assert_eq!(moved.cy, 560.0);
        assert_eq!(moved.width, start.width);
        assert_eq!(moved.height, start.height);
        assert_eq!(moved.angle_deg, start.angle_deg);

        // Clamp al borde del escaneo
        let clamped = apply_body_drag(start, (-9999.0, 9999.0), (2000, 3000));
        assert_eq!(clamped.cx, 0.0);
        assert_eq!(clamped.cy, 3000.0);
    }

    #[test]
    fn rotation_drag_updates_angle_smoothly() {
        let start = OrientedRect {
            cx: 500.0,
            cy: 500.0,
            width: 200.0,
            height: 200.0,
            angle_deg: 0.0,
        };
        // Directamente arriba -> 0°
        let r0 = apply_rotation_drag(start, (500.0, 300.0));
        assert!(r0.angle_deg.abs() < 1e-3);

        // Arriba-derecha a 45° -> +45°
        let r45 = apply_rotation_drag(start, (700.0, 300.0));
        assert!((r45.angle_deg - 45.0).abs() < 1e-3);

        // Clamp dentro de [-45, 45]
        let r_far = apply_rotation_drag(start, (900.0, 500.0));
        assert_eq!(r_far.angle_deg, 45.0);
    }

    #[test]
    fn hit_test_grabs_edge_anywhere_along_segment() {
        let viewport = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1000.0, 800.0));
        let scan_dims = (2000, 1600);
        let xform = ScanViewportTransform::fit(viewport, scan_dims);
        let crop = OrientedRect {
            cx: 1000.0,
            cy: 800.0,
            width: 1000.0,
            height: 700.0,
            angle_deg: 6.0,
        };
        let corners_screen = crop.corners().map(|c| xform.scan_to_screen(c));

        // 25% a lo largo de cada uno de los 4 segmentos de borde debe agarrar `DragHandle::Edge`, NO `Body`.
        for edge in EdgeHandle::ALL {
            let (i0, i1) = edge.corner_pair();
            let a = corners_screen[i0];
            let b = corners_screen[i1];
            let quarter_pt = a + (b - a) * 0.25;
            assert_eq!(
                hit_test_handles(crop, &xform, quarter_pt),
                Some(DragHandle::Edge(edge)),
                "expected Edge({edge:?}) at 25% along segment"
            );
        }

        // Centro debe seguir devolviendo Body.
        let center_screen = xform.scan_to_screen((crop.cx, crop.cy));
        assert_eq!(
            hit_test_handles(crop, &xform, center_screen),
            Some(DragHandle::Body)
        );
    }

    #[test]
    fn zoomed_viewport_transform_round_trips_coordinates() {
        let viewport = Rect::from_min_max(Pos2::new(40.0, 20.0), Pos2::new(1040.0, 820.0));
        let scan_dims = (5100, 7019);
        let pan = Vec2::new(-185.5, 240.25);
        let xform = ScanViewportTransform::from_view(viewport, scan_dims, 2.5, pan);

        for scan_pt in [(0.0, 0.0), (2550.0, 3509.5), (4980.25, 6812.75), (312.5, 6100.0)] {
            let screen_pt = xform.scan_to_screen(scan_pt);
            let round_tripped = xform.screen_to_scan(screen_pt);
            approx_pt(round_tripped, scan_pt, 1e-3);
        }
    }
}
