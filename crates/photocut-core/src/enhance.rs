//! Ajuste automático de color/luminosidad (restauración de viraje vintage)
//! y controles manuales no destructivos.

use image::RgbImage;
use rayon::prelude::*;

use crate::Adjustments;

/// Estadísticas de niveles por canal calculadas sobre el recorte (ignorando borde exterior).
#[derive(Debug, Clone, Copy)]
pub struct ChannelLevels {
    pub low: [f32; 3],
    pub high: [f32; 3],
    /// Mediana normalizada (0..1) por canal tras estirar entre `low` y `high`.
    pub mid: [f32; 3],
}

impl ChannelLevels {
    /// Calcula los percentiles 0.3%, 50% y 99.7% por canal R, G, B.
    pub fn analyze(img: &RgbImage) -> Self {
        let (w, h) = img.dimensions();
        if w < 8 || h < 8 {
            return Self {
                low: [0.0; 3],
                high: [255.0; 3],
                mid: [0.5; 3],
            };
        }

        // Ignorar 3% del borde para que restos del borde del escáner no sesguen el punto blanco.
        let mx = ((w as f32) * 0.03) as u32;
        let my = ((h as f32) * 0.03) as u32;

        let mut hist = [[0u32; 256]; 3];
        let mut total = 0u32;
        let raw = img.as_raw();
        let stride = w as usize * 3;

        for y in my..(h - my) {
            let row = y as usize * stride;
            for x in mx..(w - mx) {
                let i = row + x as usize * 3;
                hist[0][raw[i] as usize] += 1;
                hist[1][raw[i + 1] as usize] += 1;
                hist[2][raw[i + 2] as usize] += 1;
                total += 1;
            }
        }

        let mut low = [0.0f32; 3];
        let mut high = [255.0f32; 3];
        let mut mid = [0.5f32; 3];
        let low_target = ((total as f32) * 0.003) as u32;
        let med_target = total / 2;
        let high_target = ((total as f32) * 0.997) as u32;

        for c in 0..3 {
            let mut acc = 0u32;
            let mut l = 0u8;
            let mut m = 128u8;
            let mut hi = 255u8;
            let mut found_l = false;
            let mut found_m = false;
            for v in 0..256 {
                acc += hist[c][v];
                if !found_l && acc >= low_target {
                    l = v as u8;
                    found_l = true;
                }
                if !found_m && acc >= med_target {
                    m = v as u8;
                    found_m = true;
                }
                if acc >= high_target {
                    hi = v as u8;
                    break;
                }
            }
            if hi.saturating_sub(l) >= 16 {
                low[c] = l as f32;
                high[c] = hi as f32;
                mid[c] = ((m as f32 - low[c]) / (high[c] - low[c]).max(1.0)).clamp(0.05, 0.95);
            } else {
                mid[c] = (m as f32 / 255.0).clamp(0.05, 0.95);
            }
        }

        Self { low, high, mid }
    }
}

/// Aplica auto-color + ajustes manuales sobre una imagen (in-place, paralelizado).
pub fn apply_adjustments(img: &mut RgbImage, adj: Adjustments, levels: Option<ChannelLevels>) {
    let levels = if adj.auto_color_strength > 0.001 {
        Some(levels.unwrap_or_else(|| ChannelLevels::analyze(img)))
    } else {
        None
    };

    let auto_k = adj.auto_color_strength.clamp(0.0, 1.0);
    let contrast_factor = (1.0 + adj.contrast.clamp(-0.9, 1.5)).max(0.1);
    let brightness = adj.brightness.clamp(-1.0, 1.0) * 0.45;
    let sat = (1.0 + adj.saturation.clamp(-1.0, 1.5)).max(0.0);
    let temp = adj.temperature.clamp(-1.0, 1.0) * 0.18;
    let tint = adj.tint.clamp(-1.0, 1.0) * 0.18;

    // Gamma suave de medios tonos por canal para neutralizar virajes magenta/vintage.
    let mid_gamma = if let Some(lv) = levels {
        let target_m = ((lv.mid[0] + lv.mid[1] + lv.mid[2]) / 3.0).clamp(0.05, 0.95);
        let ln_target = target_m.ln();
        [0, 1, 2].map(|c| {
            let m_c = lv.mid[c].clamp(0.05, 0.95);
            (ln_target / m_c.ln()).clamp(0.86, 1.16)
        })
    } else {
        [1.0; 3]
    };
    let mid_blend = 0.5 * auto_k;

    // Precomputar LUT por canal para auto-niveles + medios tonos + temperatura/tinte + brillo/contraste.
    let mut lut = [[0.0f32; 256]; 3];
    let wb_gain = [1.0 + temp + tint * 0.5, 1.0 - tint, 1.0 - temp + tint * 0.5];

    for c in 0..3 {
        let gamma_c = mid_gamma[c];
        for v in 0..256 {
            let orig = v as f32 / 255.0;
            let mut val = if let Some(lv) = levels {
                let stretched = ((v as f32 - lv.low[c]) / (lv.high[c] - lv.low[c]).max(1.0))
                    .clamp(0.0, 1.0);
                let balanced = if stretched > 0.0 {
                    stretched.powf(gamma_c)
                } else {
                    0.0
                };
                let restored = stretched + (balanced - stretched) * mid_blend;
                orig * (1.0 - auto_k) + restored * auto_k
            } else {
                orig
            };

            val *= wb_gain[c];
            val = (val - 0.5) * contrast_factor + 0.5 + brightness;
            lut[c][v] = val.clamp(0.0, 1.0);
        }
    }

    let raw = img.as_mut();
    let needs_sat = (sat - 1.0).abs() > 0.001;

    raw.par_chunks_mut(3 * 1024).for_each(|chunk| {
        for px in chunk.chunks_exact_mut(3) {
            let mut r = lut[0][px[0] as usize];
            let mut g = lut[1][px[1] as usize];
            let mut b = lut[2][px[2] as usize];

            if needs_sat {
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                r = (luma + (r - luma) * sat).clamp(0.0, 1.0);
                g = (luma + (g - luma) * sat).clamp(0.0, 1.0);
                b = (luma + (b - luma) * sat).clamp(0.0, 1.0);
            }

            px[0] = (r * 255.0 + 0.5) as u8;
            px[1] = (g * 255.0 + 0.5) as u8;
            px[2] = (b * 255.0 + 0.5) as u8;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn auto_color_reduces_magenta_cast() {
        // Simula foto vintage virada al magenta: canal verde comprimido y bajo.
        let mut img = RgbImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let t = (x + y) as f32 / 126.0;
                let r = (110.0 + t * 130.0) as u8;
                let g = (40.0 + t * 110.0) as u8;
                let b = (100.0 + t * 125.0) as u8;
                img.put_pixel(x, y, Rgb([r, g, b]));
            }
        }
        let before = img.get_pixel(32, 32).0;
        let before_cast = (before[0] as i32 - before[1] as i32).abs();

        apply_adjustments(
            &mut img,
            Adjustments {
                auto_color_strength: 1.0,
                ..Default::default()
            },
            None,
        );

        let after = img.get_pixel(32, 32).0;
        let after_cast = (after[0] as i32 - after[1] as i32).abs();
        assert!(after_cast < before_cast / 2);
    }

    #[test]
    fn midtone_gamma_leaves_balanced_grayscale_neutral() {
        let mut img = RgbImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + y) * 255 / 126) as u8;
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        apply_adjustments(&mut img, Adjustments::default(), None);
        let p = img.get_pixel(32, 32).0;
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
        assert!((p[0] as i32 - 128).abs() <= 4);
    }
}
