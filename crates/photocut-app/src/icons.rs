//! Sistema de íconos vectoriales geométricos (trazo uniforme de 1.6 px) con `egui::Painter`.
//!
//! Cero emojis ni glifos Unicode inconsistentes: cada ícono se dibuja como
//! geometría vectorial nítida que hereda automáticamente el color de estado del botón.

use std::f32::consts::TAU;

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Response, Sense, Shape,
    StrokeKind, Ui, Vec2,
};

use crate::theme;

/// Catálogo completo de íconos vectoriales de PhotoCut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Home,
    FolderOpen,
    FolderOutput,
    FolderStack,
    Plus,
    Play,
    Pause,
    Eye,
    RotateLeft,
    RotateRight,
    Crop,
    Expand,
    Shrink,
    Reset,
    Check,
    Skip,
    ChevronLeft,
    ChevronRight,
    ZoomFit,
    Sparkles,
    Trash,
    Refresh,
    Close,
}

/// Variantes visuales consistentes para botones con ícono vectorial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Secondary,
    Primary,
    Amber,
    AmberFilled,
    Danger,
    Subtle,
}

/// Dibuja un ícono vectorial centrado dentro de `rect` con trazo uniforme de 1.6 px.
pub fn draw_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let origin = Pos2::new(rect.center().x - s * 0.5, rect.center().y - s * 0.5);
    let pt = |nx: f32, ny: f32| Pos2::new(origin.x + nx * s, origin.y + ny * s);
    let stroke = theme::stroke(1.6, color);
    let thin = theme::stroke(1.4, color);

    match icon {
        Icon::Home => {
            // Techo en V invertida
            painter.add(Shape::line(
                vec![pt(0.12, 0.48), pt(0.50, 0.14), pt(0.88, 0.48)],
                stroke,
            ));
            // Paredes y base
            painter.add(Shape::line(
                vec![
                    pt(0.22, 0.44),
                    pt(0.22, 0.86),
                    pt(0.78, 0.86),
                    pt(0.78, 0.44),
                ],
                stroke,
            ));
            // Puerta centrada
            painter.add(Shape::line(
                vec![
                    pt(0.41, 0.86),
                    pt(0.41, 0.58),
                    pt(0.59, 0.58),
                    pt(0.59, 0.86),
                ],
                thin,
            ));
        }
        Icon::FolderOpen => {
            // Cuerpo trasero + solapa superior
            painter.add(Shape::line(
                vec![
                    pt(0.12, 0.78),
                    pt(0.12, 0.24),
                    pt(0.40, 0.24),
                    pt(0.49, 0.34),
                    pt(0.82, 0.34),
                    pt(0.82, 0.48),
                ],
                stroke,
            ));
            // Solapa frontal abierta en perspectiva leve
            painter.add(Shape::closed_line(
                vec![
                    pt(0.12, 0.80),
                    pt(0.22, 0.48),
                    pt(0.90, 0.48),
                    pt(0.80, 0.80),
                ],
                stroke,
            ));
        }
        Icon::FolderOutput => {
            // Carpeta abierta a la derecha
            painter.add(Shape::line(
                vec![
                    pt(0.52, 0.80),
                    pt(0.14, 0.80),
                    pt(0.14, 0.22),
                    pt(0.40, 0.22),
                    pt(0.49, 0.33),
                    pt(0.82, 0.33),
                    pt(0.82, 0.45),
                ],
                stroke,
            ));
            // Flecha saliente hacia la derecha
            painter.line_segment([pt(0.50, 0.62), pt(0.88, 0.62)], stroke);
            painter.add(Shape::line(
                vec![pt(0.74, 0.48), pt(0.88, 0.62), pt(0.74, 0.76)],
                stroke,
            ));
        }
        Icon::FolderStack => {
            // Carpeta frontal
            painter.add(Shape::closed_line(
                vec![
                    pt(0.10, 0.34),
                    pt(0.35, 0.34),
                    pt(0.43, 0.43),
                    pt(0.76, 0.43),
                    pt(0.76, 0.84),
                    pt(0.10, 0.84),
                ],
                stroke,
            ));
            // Carpeta trasera apilada
            painter.add(Shape::line(
                vec![
                    pt(0.24, 0.26),
                    pt(0.24, 0.18),
                    pt(0.48, 0.18),
                    pt(0.56, 0.27),
                    pt(0.90, 0.27),
                    pt(0.90, 0.68),
                    pt(0.82, 0.68),
                ],
                thin,
            ));
        }
        Icon::Plus => {
            painter.line_segment([pt(0.50, 0.18), pt(0.50, 0.82)], stroke);
            painter.line_segment([pt(0.18, 0.50), pt(0.82, 0.50)], stroke);
        }
        Icon::Play => {
            painter.add(Shape::closed_line(
                vec![pt(0.28, 0.18), pt(0.82, 0.50), pt(0.28, 0.82)],
                stroke,
            ));
        }
        Icon::Pause => {
            let left = Rect::from_min_max(pt(0.24, 0.20), pt(0.40, 0.80));
            let right = Rect::from_min_max(pt(0.60, 0.20), pt(0.76, 0.80));
            painter.rect_stroke(left, CornerRadius::same(1), stroke, StrokeKind::Inside);
            painter.rect_stroke(right, CornerRadius::same(1), stroke, StrokeKind::Inside);
        }
        Icon::Eye => {
            // Arco superior e inferior del ojo
            let top = quadratic_curve(pt(0.10, 0.50), pt(0.50, 0.12), pt(0.90, 0.50), 10);
            let bot = quadratic_curve(pt(0.10, 0.50), pt(0.50, 0.88), pt(0.90, 0.50), 10);
            painter.add(Shape::line(top, stroke));
            painter.add(Shape::line(bot, stroke));
            painter.circle_stroke(pt(0.50, 0.50), s * 0.14, stroke);
        }
        Icon::RotateLeft => {
            // Arco desde arriba hacia la izquierda (270° horario faltando el cuadrante sup-izq)
            let arc = arc_points(pt(0.52, 0.54), s * 0.31, -0.35 * TAU, 0.42 * TAU, 16);
            painter.add(Shape::line(arc, stroke));
            // Punta de flecha apuntando en sentido antihorario arriba-izq
            painter.add(Shape::line(
                vec![pt(0.24, 0.20), pt(0.26, 0.40), pt(0.46, 0.36)],
                stroke,
            ));
        }
        Icon::RotateRight => {
            // Arco simétrico horario
            let arc = arc_points(pt(0.48, 0.54), s * 0.31, 0.08 * TAU, 0.85 * TAU, 16);
            painter.add(Shape::line(arc, stroke));
            // Punta de flecha apuntando en sentido horario arriba-der
            painter.add(Shape::line(
                vec![pt(0.76, 0.20), pt(0.74, 0.40), pt(0.54, 0.36)],
                stroke,
            ));
        }
        Icon::Crop => {
            // Escuadra vertical-horizontal izquierda-inferior
            painter.add(Shape::line(
                vec![pt(0.30, 0.12), pt(0.30, 0.70), pt(0.88, 0.70)],
                stroke,
            ));
            // Escuadra superior-derecha
            painter.add(Shape::line(
                vec![pt(0.12, 0.30), pt(0.70, 0.30), pt(0.70, 0.88)],
                stroke,
            ));
        }
        Icon::Expand => {
            // Flecha diagonal hacia arriba-derecha
            painter.line_segment([pt(0.56, 0.44), pt(0.84, 0.16)], stroke);
            painter.add(Shape::line(
                vec![pt(0.62, 0.16), pt(0.84, 0.16), pt(0.84, 0.38)],
                stroke,
            ));
            // Flecha diagonal hacia abajo-izquierda
            painter.line_segment([pt(0.44, 0.56), pt(0.16, 0.84)], stroke);
            painter.add(Shape::line(
                vec![pt(0.38, 0.84), pt(0.16, 0.84), pt(0.16, 0.62)],
                stroke,
            ));
        }
        Icon::Shrink => {
            // Flecha diagonal hacia adentro desde arriba-derecha
            painter.line_segment([pt(0.84, 0.16), pt(0.56, 0.44)], stroke);
            painter.add(Shape::line(
                vec![pt(0.56, 0.24), pt(0.56, 0.44), pt(0.76, 0.44)],
                stroke,
            ));
            // Flecha diagonal hacia adentro desde abajo-izquierda
            painter.line_segment([pt(0.16, 0.84), pt(0.44, 0.56)], stroke);
            painter.add(Shape::line(
                vec![pt(0.44, 0.76), pt(0.44, 0.56), pt(0.24, 0.56)],
                stroke,
            ));
        }
        Icon::Reset => {
            // Flecha de retorno en U
            painter.add(Shape::line(
                vec![pt(0.36, 0.20), pt(0.18, 0.36), pt(0.36, 0.52)],
                stroke,
            ));
            let arc = arc_points(pt(0.58, 0.58), s * 0.22, -0.25 * TAU, 0.25 * TAU, 10);
            let mut path = vec![pt(0.20, 0.36), pt(0.58, 0.36)];
            path.extend(arc);
            path.push(pt(0.30, 0.80));
            painter.add(Shape::line(path, stroke));
        }
        Icon::Check => {
            painter.add(Shape::line(
                vec![pt(0.16, 0.52), pt(0.40, 0.76), pt(0.84, 0.26)],
                stroke,
            ));
        }
        Icon::Skip => {
            // Triángulo avanzar + barra vertical
            painter.add(Shape::closed_line(
                vec![pt(0.18, 0.22), pt(0.62, 0.50), pt(0.18, 0.78)],
                stroke,
            ));
            painter.line_segment([pt(0.78, 0.22), pt(0.78, 0.78)], stroke);
        }
        Icon::ChevronLeft => {
            painter.add(Shape::line(
                vec![pt(0.64, 0.18), pt(0.32, 0.50), pt(0.64, 0.82)],
                stroke,
            ));
        }
        Icon::ChevronRight => {
            painter.add(Shape::line(
                vec![pt(0.36, 0.18), pt(0.68, 0.50), pt(0.36, 0.82)],
                stroke,
            ));
        }
        Icon::ZoomFit => {
            // 4 esquinas de encuadre
            painter.add(Shape::line(
                vec![pt(0.16, 0.36), pt(0.16, 0.16), pt(0.36, 0.16)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![pt(0.64, 0.16), pt(0.84, 0.16), pt(0.84, 0.36)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![pt(0.84, 0.64), pt(0.84, 0.84), pt(0.64, 0.84)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![pt(0.36, 0.84), pt(0.16, 0.84), pt(0.16, 0.64)],
                stroke,
            ));
        }
        Icon::Sparkles => {
            // Estrella geométrica de 4 puntas + destello secundario
            painter.add(Shape::closed_line(
                vec![
                    pt(0.46, 0.12),
                    pt(0.55, 0.41),
                    pt(0.84, 0.50),
                    pt(0.55, 0.59),
                    pt(0.46, 0.88),
                    pt(0.37, 0.59),
                    pt(0.08, 0.50),
                    pt(0.37, 0.41),
                ],
                thin,
            ));
            painter.line_segment([pt(0.80, 0.14), pt(0.80, 0.30)], thin);
            painter.line_segment([pt(0.72, 0.22), pt(0.88, 0.22)], thin);
        }
        Icon::Trash => {
            // Tapa y asa
            painter.line_segment([pt(0.16, 0.26), pt(0.84, 0.26)], stroke);
            painter.add(Shape::line(
                vec![
                    pt(0.36, 0.26),
                    pt(0.36, 0.15),
                    pt(0.64, 0.15),
                    pt(0.64, 0.26),
                ],
                thin,
            ));
            // Cuerpo del cesto
            painter.add(Shape::line(
                vec![
                    pt(0.24, 0.26),
                    pt(0.29, 0.85),
                    pt(0.71, 0.85),
                    pt(0.76, 0.26),
                ],
                stroke,
            ));
            painter.line_segment([pt(0.42, 0.42), pt(0.43, 0.72)], thin);
            painter.line_segment([pt(0.58, 0.42), pt(0.57, 0.72)], thin);
        }
        Icon::Refresh => {
            let arc1 = arc_points(pt(0.50, 0.50), s * 0.32, -0.42 * TAU, 0.02 * TAU, 12);
            painter.add(Shape::line(arc1, stroke));
            painter.add(Shape::line(
                vec![pt(0.70, 0.20), pt(0.84, 0.32), pt(0.68, 0.38)],
                thin,
            ));
            let arc2 = arc_points(pt(0.50, 0.50), s * 0.32, 0.08 * TAU, 0.52 * TAU, 12);
            painter.add(Shape::line(arc2, stroke));
            painter.add(Shape::line(
                vec![pt(0.30, 0.80), pt(0.16, 0.68), pt(0.32, 0.62)],
                thin,
            ));
        }
        Icon::Close => {
            painter.line_segment([pt(0.24, 0.24), pt(0.76, 0.76)], stroke);
            painter.line_segment([pt(0.76, 0.24), pt(0.24, 0.76)], stroke);
        }
    }
}

/// Botón estilizado con ícono vectorial nítido y tooltip explicativo (funciona incluso deshabilitado).
pub fn styled_icon_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    tooltip: &str,
    style: ButtonStyle,
    min_size: Vec2,
    enabled: bool,
) -> Response {
    let icon_size = if min_size.y > 0.0 && min_size.y <= 24.0 {
        13.5
    } else {
        15.0
    };
    let font_size = if min_size.y >= 36.0 {
        13.5
    } else if min_size.y > 0.0 && min_size.y <= 24.0 {
        11.5
    } else {
        12.5
    };
    let gap = if label.is_empty() { 0.0 } else { 7.0 };
    let pad_x = if label.is_empty() { 7.0 } else { 11.0 };
    let pad_y = if min_size.y > 0.0 && min_size.y <= 24.0 {
        3.0
    } else {
        5.0
    };

    let font_id = FontId::proportional(font_size);
    let galley = if label.is_empty() {
        None
    } else {
        Some(
            ui.painter()
                .layout_no_wrap(label.to_string(), font_id.clone(), Color32::WHITE),
        )
    };
    let text_w = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
    let text_h = galley.as_ref().map(|g| g.size().y).unwrap_or(0.0);

    let content_w = icon_size + gap + text_w;
    let desired = Vec2::new(
        (content_w + pad_x * 2.0).max(min_size.x),
        (text_h.max(icon_size) + pad_y * 2.0).max(min_size.y.max(28.0)),
    );

    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, mut response) = ui.allocate_exact_size(desired, sense);

    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let hovered = enabled && response.hovered();
    let pressed = enabled && response.is_pointer_button_down_on();

    let (fill, border, fg) = button_colors(style, enabled, hovered, pressed);

    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(6),
        fill,
        theme::stroke(1.0, border),
        StrokeKind::Inside,
    );

    let start_x = rect.center().x - content_w * 0.5;
    let icon_rect = Rect::from_center_size(
        Pos2::new(start_x + icon_size * 0.5, rect.center().y),
        Vec2::splat(icon_size),
    );
    draw_icon(painter, icon_rect, icon, fg);

    if !label.is_empty() {
        let final_galley = ui
            .painter()
            .layout_no_wrap(label.to_string(), font_id, fg);
        let text_pos = Pos2::new(
            start_x + icon_size + gap,
            rect.center().y - final_galley.size().y * 0.5,
        );
        painter.galley(text_pos, final_galley, fg);
    }

    if !tooltip.is_empty() {
        response = response.on_hover_text(tooltip);
    }
    response
}

pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, tooltip: &str) -> Response {
    styled_icon_button(
        ui,
        icon,
        label,
        tooltip,
        ButtonStyle::Secondary,
        Vec2::new(0.0, 28.0),
        true,
    )
}

pub fn icon_button_sized(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    tooltip: &str,
    min_size: Vec2,
    enabled: bool,
) -> Response {
    styled_icon_button(
        ui,
        icon,
        label,
        tooltip,
        ButtonStyle::Secondary,
        min_size,
        enabled,
    )
}

pub fn primary_icon_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    tooltip: &str,
    min_size: Vec2,
    enabled: bool,
) -> Response {
    styled_icon_button(
        ui,
        icon,
        label,
        tooltip,
        ButtonStyle::Primary,
        min_size,
        enabled,
    )
}

pub fn small_icon_button(ui: &mut Ui, icon: Icon, label: &str, tooltip: &str) -> Response {
    styled_icon_button(
        ui,
        icon,
        label,
        tooltip,
        ButtonStyle::Secondary,
        Vec2::new(0.0, 22.0),
        true,
    )
}

pub fn danger_icon_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    tooltip: &str,
    enabled: bool,
) -> Response {
    styled_icon_button(
        ui,
        icon,
        label,
        tooltip,
        ButtonStyle::Danger,
        Vec2::new(0.0, 28.0),
        enabled,
    )
}

pub fn subtle_icon_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    tooltip: &str,
    enabled: bool,
) -> Response {
    styled_icon_button(
        ui,
        icon,
        label,
        tooltip,
        ButtonStyle::Subtle,
        Vec2::new(0.0, 26.0),
        enabled,
    )
}

fn button_colors(
    style: ButtonStyle,
    enabled: bool,
    hovered: bool,
    pressed: bool,
) -> (Color32, Color32, Color32) {
    if !enabled {
        return (
            Color32::from_rgb(0x20, 0x22, 0x29),
            theme::BORDER,
            theme::SKIPPED,
        );
    }

    match style {
        ButtonStyle::Primary => {
            let fill = if pressed {
                Color32::from_rgb(0x25, 0x63, 0xEB)
            } else if hovered {
                theme::ACCENT_HOVER
            } else {
                theme::ACCENT
            };
            (fill, fill, Color32::WHITE)
        }
        ButtonStyle::Secondary => {
            let fill = if pressed {
                theme::BG_SURFACE
            } else if hovered {
                theme::BG_CARD_HOVER
            } else {
                theme::BG_CARD
            };
            let border = if hovered {
                theme::ACCENT_HOVER
            } else {
                theme::BORDER
            };
            (fill, border, theme::TEXT_PRIMARY)
        }
        ButtonStyle::Amber => {
            let fill = if pressed {
                Color32::from_rgba_unmultiplied(245, 158, 11, 56)
            } else if hovered {
                Color32::from_rgba_unmultiplied(245, 158, 11, 44)
            } else {
                Color32::from_rgba_unmultiplied(245, 158, 11, 24)
            };
            (fill, theme::RECROP, theme::RECROP)
        }
        ButtonStyle::AmberFilled => {
            let fill = if hovered {
                Color32::from_rgb(0xFB, 0xBF, 0x24)
            } else {
                theme::RECROP
            };
            (fill, fill, theme::BG_VIEWPORT)
        }
        ButtonStyle::Danger => {
            let fill = if hovered {
                Color32::from_rgba_unmultiplied(239, 68, 68, 34)
            } else {
                theme::BG_CARD
            };
            let border = if hovered { theme::ERROR } else { theme::BORDER };
            let fg = if hovered {
                theme::ERROR
            } else {
                theme::TEXT_SECONDARY
            };
            (fill, border, fg)
        }
        ButtonStyle::Subtle => {
            let fill = if hovered {
                theme::BG_CARD
            } else {
                Color32::TRANSPARENT
            };
            let border = if hovered {
                theme::BORDER
            } else {
                Color32::TRANSPARENT
            };
            let fg = if hovered {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            };
            (fill, border, fg)
        }
    }
}

fn arc_points(center: Pos2, radius: f32, start_rad: f32, end_rad: f32, steps: usize) -> Vec<Pos2> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let a = start_rad + (end_rad - start_rad) * t;
            Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}

fn quadratic_curve(p0: Pos2, ctrl: Pos2, p1: Pos2, steps: usize) -> Vec<Pos2> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let u = 1.0 - t;
            Pos2::new(
                u * u * p0.x + 2.0 * u * t * ctrl.x + t * t * p1.x,
                u * u * p0.y + 2.0 * u * t * ctrl.y + t * t * p1.y,
            )
        })
        .collect()
}
