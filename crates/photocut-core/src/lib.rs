//! Núcleo de procesamiento de PhotoCut (independiente de la interfaz gráfica).
//!
//! Mantiene todas las ediciones de forma no destructiva en coordenadas del
//! escaneo original (ej. 600 DPI), permitiendo re-recortar sin pérdida de calidad
//! antes de exportar.

pub mod cache;
pub mod detect;
pub mod enhance;
pub mod export;
pub mod jobs;
pub mod orient;
pub mod session;

use serde::{Deserialize, Serialize};

/// Rectángulo orientado en coordenadas de píxeles del escaneo original.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OrientedRect {
    /// Centro X en el escaneo original (px).
    pub cx: f32,
    /// Centro Y en el escaneo original (px).
    pub cy: f32,
    /// Ancho del recorte antes de rotar (px).
    pub width: f32,
    /// Alto del recorte antes de rotar (px).
    pub height: f32,
    /// Ángulo fino de enderezado en grados (deskew, típico -45°..45°).
    pub angle_deg: f32,
}

impl OrientedRect {
    /// Expande o contrae el recorte en píxeles por lado (útil para "volver atrás"
    /// si el auto-recorte comió parte del borde).
    pub fn expanded(self, margin_px: f32) -> Self {
        Self {
            width: (self.width + margin_px * 2.0).max(1.0),
            height: (self.height + margin_px * 2.0).max(1.0),
            ..self
        }
    }

    /// Devuelve las 4 esquinas (TL, TR, BR, BL) en coordenadas del escaneo original.
    pub fn corners(self) -> [(f32, f32); 4] {
        let rad = self.angle_deg.to_radians();
        let (sin_a, cos_a) = rad.sin_cos();
        let hw = self.width * 0.5;
        let hh = self.height * 0.5;
        let locals = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
        locals.map(|(lx, ly)| {
            (
                self.cx + lx * cos_a - ly * sin_a,
                self.cy + lx * sin_a + ly * cos_a,
            )
        })
    }
}

/// Rotación gruesa de 90° en sentido horario para poner derecha la foto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QuarterTurn {
    #[default]
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl QuarterTurn {
    pub fn rotate_cw(self) -> Self {
        match self {
            Self::Deg0 => Self::Deg90,
            Self::Deg90 => Self::Deg180,
            Self::Deg180 => Self::Deg270,
            Self::Deg270 => Self::Deg0,
        }
    }

    pub fn rotate_ccw(self) -> Self {
        match self {
            Self::Deg0 => Self::Deg270,
            Self::Deg90 => Self::Deg0,
            Self::Deg180 => Self::Deg90,
            Self::Deg270 => Self::Deg180,
        }
    }

    pub fn degrees(self) -> f32 {
        match self {
            Self::Deg0 => 0.0,
            Self::Deg90 => 90.0,
            Self::Deg180 => 180.0,
            Self::Deg270 => 270.0,
        }
    }
}

/// Parámetros de ajuste no destructivos aplicados al recorte.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Adjustments {
    /// Intensidad de restauración automática de color/niveles (0.0 = sin cambio, 1.0 = completo).
    pub auto_color_strength: f32,
    /// Exposición / brillo (-1.0..1.0).
    pub brightness: f32,
    /// Contraste (-1.0..1.0).
    pub contrast: f32,
    /// Saturación (-1.0..1.0).
    pub saturation: f32,
    /// Temperatura de color frío/cálido (-1.0..1.0).
    pub temperature: f32,
    /// Tinte verde/magenta (-1.0..1.0, útil para fotos viejas viradas al magenta).
    pub tint: f32,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self {
            auto_color_strength: 0.7,
            brightness: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            temperature: 0.0,
            tint: 0.0,
        }
    }
}

/// Formato de guardado final configurable por el usuario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportFormat {
    /// PNG sin pérdida (conserva 100% del detalle del escaneo).
    Png,
    /// TIFF sin pérdida (estándar de archivo fotográfico).
    Tiff,
    /// JPEG alta calidad con calidad configurable (80..=100, por defecto 96).
    Jpeg { quality: u8 },
}

impl Default for ExportFormat {
    fn default() -> Self {
        Self::Png
    }
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Tiff => "tif",
            Self::Jpeg { .. } => "jpg",
        }
    }
}

/// Estado de revisión humana de una foto detectada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReviewStatus {
    #[default]
    Pending,
    Approved,
    Skipped,
}

/// Foto individual detectada dentro de un escaneo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectedPhoto {
    pub id: usize,
    pub crop: OrientedRect,
    pub turn: QuarterTurn,
    pub adjustments: Adjustments,
    pub status: ReviewStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_turn_cycles_full_circle() {
        let mut t = QuarterTurn::Deg0;
        for _ in 0..4 {
            t = t.rotate_cw();
        }
        assert_eq!(t, QuarterTurn::Deg0);
        assert_eq!(t.rotate_ccw(), QuarterTurn::Deg270);
    }

    #[test]
    fn expanded_rect_never_collapses_below_one_pixel() {
        let rect = OrientedRect {
            cx: 500.0,
            cy: 500.0,
            width: 100.0,
            height: 80.0,
            angle_deg: -1.5,
        };
        let grown = rect.expanded(20.0);
        assert_eq!(grown.width, 140.0);
        assert_eq!(grown.height, 120.0);

        let shrunk = rect.expanded(-500.0);
        assert_eq!(shrunk.width, 1.0);
        assert_eq!(shrunk.height, 1.0);
    }

    #[test]
    fn corners_match_axis_aligned_rect() {
        let rect = OrientedRect {
            cx: 100.0,
            cy: 200.0,
            width: 40.0,
            height: 20.0,
            angle_deg: 0.0,
        };
        let c = rect.corners();
        assert_eq!(c[0], (80.0, 190.0));
        assert_eq!(c[1], (120.0, 190.0));
        assert_eq!(c[2], (120.0, 210.0));
        assert_eq!(c[3], (80.0, 210.0));
    }
}
