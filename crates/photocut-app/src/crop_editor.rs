//! Editor interactivo de Re-recortar directamente sobre el escaneo original.
//!
//! Opera en coordenadas exactas de píxeles del escaneo original (600 DPI) usando
//! una transformación limpia pantalla <-> escaneo y garantiza invariantes geométricos
//! (esquina/borde opuesto fijo y tamaño mínimo de recorte).

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    TextureHandle, Vec2,
};
use photocut_core::OrientedRect;

use crate::theme;

/// Tamaño mínimo permitido en píxeles del escaneo original para evitar colapsos.
pub const MIN_CROP_SIZE: f32 = 32.0;

const HANDLE_HIT_RADIUS: f32 = 13.0;
const EDGE_HIT_RADIUS: f32 = 12.0;
const ROTATE_STEM_SCREEN_PX: f32 = 28.0;
const VIEWPORT_PADDING_PX: f32 = 40.0;

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
struct ActiveDrag {
    handle: DragHandle,
    start_crop: OrientedRect,
    start_scan_pos: (f32, f32),
}

/// Estado persistente de interacción del editor de recorte.
#[derive(Debug, Default)]
pub struct CropEditorState {
    active_drag: Option<ActiveDrag>,
}

impl CropEditorState {
    pub fn clear_drag(&mut self) {
        self.active_drag = None;
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
        let sw = (scan_dims.0 as f32).max(1.0);
        let sh = (scan_dims.1 as f32).max(1.0);
        let avail_w = (viewport.width() - VIEWPORT_PADDING_PX * 2.0).max(64.0);
        let avail_h = (viewport.height() - VIEWPORT_PADDING_PX * 2.0).max(64.0);
        let scale = (avail_w / sw).min(avail_h / sh);
        let disp_w = sw * scale;
        let disp_h = sh * scale;
        let screen_rect = Rect::from_center_size(viewport.center(), Vec2::new(disp_w, disp_h));
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

/// Renderiza el escaneo completo con la máscara oscura y controles interactivos de `crop`.
pub fn show_crop_editor(
    ui: &mut egui::Ui,
    state: &mut CropEditorState,
    backdrop_tex: &TextureHandle,
    orig_scan_dims: (u32, u32),
    crop: &mut OrientedRect,
    sibling_crops: &[(usize, OrientedRect)],
) -> CropEditOutcome {
    let available = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(available, Sense::click_and_drag());
    let xform = ScanViewportTransform::fit(available, orig_scan_dims);

    let mut outcome = CropEditOutcome::default();

    // 1. Procesar interacción del puntero.
    let hovered_handle = response
        .hover_pos()
        .and_then(|pos| hit_test_handles(*crop, &xform, pos));

    if response.drag_started()
        && let Some(press_pos) = response.interact_pointer_pos()
        && let Some(handle) = hit_test_handles(*crop, &xform, press_pos)
    {
        state.active_drag = Some(ActiveDrag {
            handle,
            start_crop: *crop,
            start_scan_pos: xform.screen_to_scan(press_pos),
        });
    }

    if response.dragged()
        && let Some(drag) = state.active_drag
        && let Some(curr_pos) = response.interact_pointer_pos()
    {
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

    if response.drag_stopped() && state.active_drag.take().is_some() {
        outcome.drag_finished = true;
    }

    // Cursor contextual según handle activo o bajo el puntero.
    let cursor_handle = state.active_drag.map(|d| d.handle).or(hovered_handle);
    if let Some(h) = cursor_handle {
        let icon = match h {
            DragHandle::Corner(c) => c.cursor(),
            DragHandle::Edge(e) => e.cursor(),
            DragHandle::Body => {
                if state.active_drag.is_some() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                }
            }
            DragHandle::Rotate => CursorIcon::PointingHand,
        };
        ui.ctx().set_cursor_icon(icon);
    }

    // 2. Dibujar fondo del escaneo completo.
    let painter = ui.painter_at(available);
    painter.rect_filled(available, CornerRadius::ZERO, theme::BG_VIEWPORT);

    // Sombra sutil y borde alrededor del escaneo.
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

    // 3. Máscara oscura fuera del rectángulo orientado (recortada al área del escaneo).
    draw_dimmed_outside_mask(&painter, xform.screen_rect, *crop, &xform);

    // 4. Contornos tenues de otras fotos detectadas en el mismo escaneo para referencia espacial.
    for &(other_id, other_rect) in sibling_crops {
        let pts: Vec<Pos2> = other_rect
            .corners()
            .into_iter()
            .map(|c| xform.scan_to_screen(c))
            .collect();
        painter.add(Shape::closed_line(
            pts.clone(),
            theme::stroke(1.0, Color32::from_rgba_unmultiplied(150, 156, 168, 120)),
        ));
        if let Some(&tl) = pts.first() {
            painter.text(
                tl + Vec2::new(6.0, 6.0),
                egui::Align2::LEFT_TOP,
                format!("Foto {}", other_id + 1),
                egui::FontId::proportional(11.0),
                Color32::from_rgba_unmultiplied(236, 238, 242, 140),
            );
        }
    }

    // 5. Guías de regla de tercios dentro del recorte activo.
    draw_rule_of_thirds(&painter, *crop, &xform);

    // 6. Borde nítido ámbar + filete interior cian sutil.
    let corners_screen: Vec<Pos2> = crop
        .corners()
        .into_iter()
        .map(|c| xform.scan_to_screen(c))
        .collect();
    painter.add(Shape::closed_line(
        corners_screen.clone(),
        theme::stroke(2.2, theme::RECROP),
    ));
    let inner_crop = crop.expanded((-1.5 / xform.scale).min(-0.5));
    let inner_screen: Vec<Pos2> = inner_crop
        .corners()
        .into_iter()
        .map(|c| xform.scan_to_screen(c))
        .collect();
    painter.add(Shape::closed_line(
        inner_screen,
        theme::stroke(1.0, Color32::from_rgba_unmultiplied(56, 189, 248, 150)),
    ));

    // 7. Dibujar handles de rotación, bordes y esquinas.
    let active_or_hover = state.active_drag.map(|d| d.handle).or(hovered_handle);
    draw_handles(&painter, *crop, &xform, active_or_hover);

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

fn hit_test_handles(
    crop: OrientedRect,
    xform: &ScanViewportTransform,
    pointer: Pos2,
) -> Option<DragHandle> {
    // 1. Handle superior de rotación.
    let (_, rot_knob) = rotation_handle_screen(crop, xform);
    if pointer.distance(rot_knob) <= HANDLE_HIT_RADIUS {
        return Some(DragHandle::Rotate);
    }

    // 2. Las 4 esquinas.
    let corners = crop.corners();
    for corner in CornerHandle::ALL {
        let p = xform.scan_to_screen(corners[corner.corner_index()]);
        if pointer.distance(p) <= HANDLE_HIT_RADIUS {
            return Some(DragHandle::Corner(corner));
        }
    }

    // 3. Los 4 puntos medios de borde.
    for edge in EdgeHandle::ALL {
        let p = xform.scan_to_screen(edge_midpoint_scan(crop, edge));
        if pointer.distance(p) <= EDGE_HIT_RADIUS {
            return Some(DragHandle::Edge(edge));
        }
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

    let dim = Color32::from_rgba_unmultiplied(10, 12, 16, 165);
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

    let guide_stroke = theme::stroke(1.0, Color32::from_rgba_unmultiplied(236, 238, 242, 52));
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
    let dark_outline = theme::stroke(1.5, theme::BG_VIEWPORT);

    // Handle de rotación superior
    let (top_mid, rot_knob) = rotation_handle_screen(crop, xform);
    let rot_active = highlighted == Some(DragHandle::Rotate);
    painter.line_segment(
        [top_mid, rot_knob],
        theme::stroke(1.5, if rot_active { Color32::WHITE } else { theme::RECROP }),
    );
    painter.circle(
        rot_knob,
        if rot_active { 7.5 } else { 6.0 },
        if rot_active { Color32::WHITE } else { theme::RECROP },
        dark_outline,
    );

    // 4 handles de punto medio de borde (cápsulas orientadas)
    let (u, v) = rect_axes(crop.angle_deg);
    for edge in EdgeHandle::ALL {
        let mid = xform.scan_to_screen(edge_midpoint_scan(crop, edge));
        let active = highlighted == Some(DragHandle::Edge(edge));
        let dir = match edge {
            EdgeHandle::Top | EdgeHandle::Bottom => Vec2::new(u.0, u.1),
            EdgeHandle::Left | EdgeHandle::Right => Vec2::new(v.0, v.1),
        };
        let half_len = if active { 11.0 } else { 9.0 };
        let color = if active { Color32::WHITE } else { theme::RECROP };
        painter.line_segment(
            [mid - dir * half_len, mid + dir * half_len],
            theme::stroke(6.0, theme::BG_VIEWPORT),
        );
        painter.line_segment(
            [mid - dir * half_len, mid + dir * half_len],
            theme::stroke(3.5, color),
        );
    }

    // 4 handles de esquina
    let corners = crop.corners();
    for corner in CornerHandle::ALL {
        let p = xform.scan_to_screen(corners[corner.corner_index()]);
        let active = highlighted == Some(DragHandle::Corner(corner));
        let half = if active { 6.5 } else { 5.2 };
        let rect = Rect::from_center_size(p, Vec2::splat(half * 2.0));
        painter.rect(
            rect,
            CornerRadius::same(2),
            if active { Color32::WHITE } else { theme::RECROP },
            dark_outline,
            StrokeKind::Outside,
        );
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
        approx_pt(edge_midpoint_scan(expanded, EdgeHandle::Left), orig_left_mid, 1e-3);

        // Comprimir borde superior más allá del inferior -> se detiene en MIN_CROP_SIZE y borde inferior intacto.
        let orig_bottom_mid = edge_midpoint_scan(start, EdgeHandle::Bottom);
        let shrunk = apply_edge_drag(start, EdgeHandle::Top, (800.0, 1800.0));
        assert_eq!(shrunk.height, MIN_CROP_SIZE);
        assert_eq!(shrunk.width, start.width);
        approx_pt(edge_midpoint_scan(shrunk, EdgeHandle::Bottom), orig_bottom_mid, 1e-3);
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
}
