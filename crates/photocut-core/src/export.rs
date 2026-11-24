//! Extracción en un solo paso desde el escaneo original (600 DPI) y guardado en máxima calidad.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{ImageError, ImageFormat, RgbImage};
use rayon::prelude::*;

use crate::enhance::{ChannelLevels, apply_adjustments};
use crate::{Adjustments, ExportFormat, OrientedRect, QuarterTurn};

/// Modo de remuestreo: `Bilinear` para preview en tiempo real, `Bicubic` (Catmull-Rom) para export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleQuality {
    Bilinear,
    Bicubic,
}

/// Extrae y endereza una foto en **una sola pasada** desde el escaneo original (sin doble pérdida).
/// Si `max_output_dim` es `Some(n)`, escala el resultado a ese lado mayor (ideal para previews UI).
pub fn extract_photo(
    scan: &RgbImage,
    crop: OrientedRect,
    turn: QuarterTurn,
    adjustments: Adjustments,
    quality: SampleQuality,
    max_output_dim: Option<u32>,
) -> RgbImage {
    let base_w = crop.width.round().max(1.0);
    let base_h = crop.height.round().max(1.0);

    // Intercambiar ancho/alto si la rotación gruesa es 90° o 270°.
    let (rot_w, rot_h) = match turn {
        QuarterTurn::Deg0 | QuarterTurn::Deg180 => (base_w, base_h),
        QuarterTurn::Deg90 | QuarterTurn::Deg270 => (base_h, base_w),
    };

    let scale = match max_output_dim {
        Some(limit) if rot_w.max(rot_h) > limit as f32 => limit as f32 / rot_w.max(rot_h),
        _ => 1.0,
    };

    let out_w = (rot_w * scale).round().max(1.0) as u32;
    let out_h = (rot_h * scale).round().max(1.0) as u32;

    // Mapeo inverso: píxel de salida -> coordenadas del escaneo original.
    // Rotación total: ángulo del rectángulo menos la rotación horaria deseada por el usuario.
    let total_rad = (crop.angle_deg - turn.degrees()).to_radians();
    let (sin_a, cos_a) = total_rad.sin_cos();

    let inv_scale = 1.0 / scale;
    let half_out_w = (out_w as f32 - 1.0) * 0.5;
    let half_out_h = (out_h as f32 - 1.0) * 0.5;

    let (sw, sh) = scan.dimensions();
    let src_raw = scan.as_raw();
    let mut out_raw = vec![0u8; out_w as usize * out_h as usize * 3];

    out_raw
        .par_chunks_mut(out_w as usize * 3)
        .enumerate()
        .for_each(|(oy, row)| {
            let ly = (oy as f32 - half_out_h) * inv_scale;
            for ox in 0..out_w as usize {
                let lx = (ox as f32 - half_out_w) * inv_scale;
                let sx = crop.cx + lx * cos_a - ly * sin_a;
                let sy = crop.cy + lx * sin_a + ly * cos_a;

                let rgb = match quality {
                    SampleQuality::Bilinear => sample_bilinear(src_raw, sw, sh, sx, sy),
                    SampleQuality::Bicubic => sample_catmull_rom(src_raw, sw, sh, sx, sy),
                };
                let dst = ox * 3;
                row[dst] = rgb[0];
                row[dst + 1] = rgb[1];
                row[dst + 2] = rgb[2];
            }
        });

    let mut out = RgbImage::from_raw(out_w, out_h, out_raw).expect("valid output buffer");
    let levels = if adjustments.auto_color_strength > 0.001 {
        Some(ChannelLevels::analyze(&out))
    } else {
        None
    };
    apply_adjustments(&mut out, adjustments, levels);
    out
}

/// Guarda una imagen ya procesada en el formato elegido en máxima calidad.
pub fn save_image(img: &RgbImage, path: &Path, format: ExportFormat) -> Result<(), ImageError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match format {
        ExportFormat::Png => img.save_with_format(path, ImageFormat::Png),
        ExportFormat::Tiff => img.save_with_format(path, ImageFormat::Tiff),
        ExportFormat::Jpeg { quality } => {
            let file = File::create(path)?;
            let writer = BufWriter::new(file);
            let q = quality.clamp(80, 100);
            let mut enc = JpegEncoder::new_with_quality(writer, q);
            enc.encode(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8)
        }
    }
}

#[inline]
fn get_clamped(raw: &[u8], w: i32, h: i32, x: i32, y: i32) -> [f32; 3] {
    let cx = x.clamp(0, w - 1) as usize;
    let cy = y.clamp(0, h - 1) as usize;
    let idx = (cy * w as usize + cx) * 3;
    [raw[idx] as f32, raw[idx + 1] as f32, raw[idx + 2] as f32]
}

#[inline]
fn sample_bilinear(raw: &[u8], w: u32, h: u32, sx: f32, sy: f32) -> [u8; 3] {
    let x0 = sx.floor() as i32;
    let y0 = sy.floor() as i32;
    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;

    let wi = w as i32;
    let hi = h as i32;
    let p00 = get_clamped(raw, wi, hi, x0, y0);
    let p10 = get_clamped(raw, wi, hi, x0 + 1, y0);
    let p01 = get_clamped(raw, wi, hi, x0, y0 + 1);
    let p11 = get_clamped(raw, wi, hi, x0 + 1, y0 + 1);

    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] + (p10[c] - p00[c]) * fx;
        let bot = p01[c] + (p11[c] - p01[c]) * fx;
        out[c] = (top + (bot - top) * fy).clamp(0.0, 255.0) as u8;
    }
    out
}

#[inline]
fn cubic_weight(t: f32) -> f32 {
    let a = t.abs();
    if a <= 1.0 {
        1.5 * a * a * a - 2.5 * a * a + 1.0
    } else if a < 2.0 {
        -0.5 * a * a * a + 2.5 * a * a - 4.0 * a + 2.0
    } else {
        0.0
    }
}

#[inline]
fn sample_catmull_rom(raw: &[u8], w: u32, h: u32, sx: f32, sy: f32) -> [u8; 3] {
    let ix = sx.floor() as i32;
    let iy = sy.floor() as i32;
    let fx = sx - ix as f32;
    let fy = sy - iy as f32;

    let wx = [
        cubic_weight(fx + 1.0),
        cubic_weight(fx),
        cubic_weight(1.0 - fx),
        cubic_weight(2.0 - fx),
    ];
    let wy = [
        cubic_weight(fy + 1.0),
        cubic_weight(fy),
        cubic_weight(1.0 - fy),
        cubic_weight(2.0 - fy),
    ];

    let wi = w as i32;
    let hi = h as i32;
    let mut acc = [0.0f32; 3];

    for j in 0..4 {
        let y = iy + j as i32 - 1;
        let wyj = wy[j];
        for i in 0..4 {
            let x = ix + i as i32 - 1;
            let weight = wx[i] * wyj;
            let p = get_clamped(raw, wi, hi, x, y);
            acc[0] += p[0] * weight;
            acc[1] += p[1] * weight;
            acc[2] += p[2] * weight;
        }
    }

    [
        acc[0].clamp(0.0, 255.0) as u8,
        acc[1].clamp(0.0, 255.0) as u8,
        acc[2].clamp(0.0, 255.0) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn extract_90_cw_swaps_dimensions_and_rotates_corner() {
        let mut scan = RgbImage::from_pixel(100, 60, Rgb([10, 10, 10]));
        // Pintar esquina superior izquierda de rojo
        for y in 0..10 {
            for x in 0..10 {
                scan.put_pixel(x, y, Rgb([250, 0, 0]));
            }
        }
        let rect = OrientedRect {
            cx: 49.5,
            cy: 29.5,
            width: 100.0,
            height: 60.0,
            angle_deg: 0.0,
        };
        let out = extract_photo(
            &scan,
            rect,
            QuarterTurn::Deg90,
            Adjustments {
                auto_color_strength: 0.0,
                ..Default::default()
            },
            SampleQuality::Bilinear,
            None,
        );
        assert_eq!(out.dimensions(), (60, 100));
        // Al rotar 90° horario, la esquina superior izquierda pasa a ser la superior derecha.
        let top_right = out.get_pixel(55, 4).0;
        assert!(top_right[0] > 200 && top_right[1] < 20);
    }
}
