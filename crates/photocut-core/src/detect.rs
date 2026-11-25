//! Detección automática de múltiples fotos dentro de un escaneo sobre fondo blanco.
//!
//! Diseñado para escaneos de alta resolución (ej. 600 DPI / 5100×7019 px):
//! trabaja sobre una copia reducida rápida (~1200 px) y devuelve rectángulos
//! orientados (`OrientedRect`) en coordenadas exactas del escaneo original.

use image::RgbImage;
use rayon::prelude::*;

use crate::{Adjustments, DetectedPhoto, OrientedRect, QuarterTurn, ReviewStatus};

/// Configuración del detector multi-foto.
#[derive(Debug, Clone, Copy)]
pub struct DetectConfig {
    /// Altura/lado mayor máximo para la imagen de trabajo en detección.
    pub work_max_dim: u32,
    /// Fracción mínima del área total del escaneo para considerar un recorte válido.
    pub min_area_ratio: f32,
    /// Distancia mínima al blanco de fondo (por canal) para marcar foreground.
    pub bg_diff_threshold: u8,
    /// Umbral de saturación/croma (`max - min` entre canales) para rescatar tonos claros.
    pub chroma_threshold: u8,
    /// Margen de seguridad (px en imagen original): positivo expande, negativo recorta pelitos blancos.
    pub margin_px: f32,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            work_max_dim: 1200,
            min_area_ratio: 0.018,
            bg_diff_threshold: 18,
            chroma_threshold: 10,
            margin_px: -8.0,
        }
    }
}

/// Detecta todas las fotos individuales presentes en `scan`.
pub fn detect_photos(scan: &RgbImage, cfg: DetectConfig) -> Vec<DetectedPhoto> {
    let (orig_w, orig_h) = scan.dimensions();
    if orig_w < 32 || orig_h < 32 {
        return Vec::new();
    }

    let (work, scale_x, scale_y) = downsample_box(scan, cfg.work_max_dim);
    let (w, h) = (work.width() as usize, work.height() as usize);

    let global_bg = estimate_background_rgb(&work);
    let (bg_col, bg_row) = estimate_local_background_profiles(&work, global_bg);

    // Padding blanco sintético para fotos pegadas al borde del escáner (ej. img003, img040).
    const PAD: usize = 6;
    let pw = w + PAD * 2;
    let ph = h + PAD * 2;
    let mut mask = vec![0u8; pw * ph];

    let raw = work.as_raw();
    for y in 0..h {
        let dst_row = (y + PAD) * pw + PAD;
        let src_row = y * w * 3;
        let row_bg = bg_row[y];
        for x in 0..w {
            let idx = src_row + x * 3;
            let r = raw[idx];
            let g = raw[idx + 1];
            let b = raw[idx + 2];

            let col_bg = bg_col[x];
            let local_r = (col_bg[0] as i32 + row_bg[0] as i32 - global_bg[0] as i32)
                .clamp(185, 255) as u8;
            let local_g = (col_bg[1] as i32 + row_bg[1] as i32 - global_bg[1] as i32)
                .clamp(185, 255) as u8;
            let local_b = (col_bg[2] as i32 + row_bg[2] as i32 - global_bg[2] as i32)
                .clamp(185, 255) as u8;

            let diff = local_r
                .saturating_sub(r)
                .max(local_g.saturating_sub(g))
                .max(local_b.saturating_sub(b));
            let chroma = r.max(g).max(b) - r.min(g).min(b);

            if diff >= cfg.bg_diff_threshold || chroma >= cfg.chroma_threshold {
                mask[dst_row + x] = 1;
            }
        }
    }

    // Apertura 1px: elimina motas de polvo en el vidrio sin afectar fotos.
    mask = erode_3x3(&mask, pw, ph);
    mask = dilate_3x3(&mask, pw, ph);

    // Cierre 2px: une micro-grietas claras sin fusionar fotos separadas por ~12-15px.
    mask = dilate_3x3(&mask, pw, ph);
    mask = dilate_3x3(&mask, pw, ph);
    mask = erode_3x3(&mask, pw, ph);
    mask = erode_3x3(&mask, pw, ph);

    // Relleno de huecos internos (ej. ropa blanca o paredes blancas rodeadas por la foto).
    fill_internal_holes(&mut mask, pw, ph);

    // Componentes conectados (4-conectividad).
    let components = extract_components(&mask, pw, ph);
    let min_pixels = ((w * h) as f32 * cfg.min_area_ratio) as usize;
    let min_side = (w.min(h) as f32) * 0.06;

    let mut rects: Vec<OrientedRect> = Vec::new();
    for comp in components {
        if comp.len() < min_pixels {
            continue;
        }
        // Extraer puntos de borde del componente para convex hull rápido.
        let boundary = component_boundary_points(&comp, &mask, pw, ph);
        if boundary.len() < 4 {
            continue;
        }
        let hull = convex_hull(&boundary);
        if hull.len() < 3 {
            continue;
        }

        let Some(work_rect) = min_area_bounding_rect(&hull) else {
            continue;
        };
        if work_rect.width < min_side || work_rect.height < min_side {
            continue;
        }

        // Volver a coordenadas del escaneo original (quitando PAD y multiplicando por escala).
        let cx = (work_rect.cx - PAD as f32) * scale_x;
        let cy = (work_rect.cy - PAD as f32) * scale_y;
        let width = (work_rect.width * scale_x).min(orig_w as f32);
        let height = (work_rect.height * scale_y).min(orig_h as f32);

        let rect = OrientedRect {
            cx: cx.clamp(0.0, orig_w as f32),
            cy: cy.clamp(0.0, orig_h as f32),
            width,
            height,
            angle_deg: normalize_deskew_angle(work_rect.angle_deg),
        };

        rects.push(rect);
    }

    // Filtro de seguridad: descartar rectángulos gigantes que envuelven otra foto detectada.
    rects = drop_wrapping_artifacts(rects);

    // Etapa 2: refinamiento de ángulo fino (Theil-Sen) y recorte independiente de bordes blancos/sombras.
    let base_inset_px = (-cfg.margin_px).clamp(0.0, 6.0);
    let extra_expand_px = cfg.margin_px.max(0.0);
    rects = rects
        .into_par_iter()
        .map(|coarse| {
            let mut refined = refine_oriented_rect_with_inset(scan, coarse, base_inset_px);
            if extra_expand_px > 0.0 {
                refined = refined.expanded(extra_expand_px);
            }
            refined
        })
        .collect();

    // Ordenar en orden natural de lectura: filas de arriba hacia abajo, columnas izq -> der.
    let row_tolerance = (orig_h as f32) * 0.12;
    rects.sort_by(|a, b| {
        if (a.cy - b.cy).abs() < row_tolerance {
            a.cx.total_cmp(&b.cx)
        } else {
            a.cy.total_cmp(&b.cy)
        }
    });

    rects
        .into_iter()
        .enumerate()
        .map(|(id, crop)| DetectedPhoto {
            id,
            crop,
            turn: QuarterTurn::Deg0,
            adjustments: Adjustments::default(),
            status: ReviewStatus::Pending,
        })
        .collect()
}

/// Downsample rápido por promediado de caja paralelizado con Rayon.
pub(crate) fn downsample_box(src: &RgbImage, max_dim: u32) -> (RgbImage, f32, f32) {
    let (sw, sh) = src.dimensions();
    let longest = sw.max(sh);
    if longest <= max_dim {
        return (src.clone(), 1.0, 1.0);
    }
    let scale = max_dim as f32 / longest as f32;
    let dw = ((sw as f32 * scale).round() as u32).max(1);
    let dh = ((sh as f32 * scale).round() as u32).max(1);

    let sx_ratio = sw as f32 / dw as f32;
    let sy_ratio = sh as f32 / dh as f32;

    let src_raw = src.as_raw();
    let sw_usize = sw as usize;
    let dw_usize = dw as usize;
    let mut dst_raw = vec![0u8; dw_usize * dh as usize * 3];

    dst_raw
        .par_chunks_mut(dw_usize * 3)
        .enumerate()
        .for_each(|(dy, row)| {
            let y0 = (dy as f32 * sy_ratio) as usize;
            let y1 = (((dy + 1) as f32 * sy_ratio).ceil() as usize).min(sh as usize).max(y0 + 1);
            for dx in 0..dw_usize {
                let x0 = (dx as f32 * sx_ratio) as usize;
                let x1 = (((dx + 1) as f32 * sx_ratio).ceil() as usize).min(sw_usize).max(x0 + 1);
                let mut r = 0u32;
                let mut g = 0u32;
                let mut b = 0u32;
                let mut count = 0u32;
                for sy in y0..y1 {
                    let base = sy * sw_usize * 3;
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

    let img = RgbImage::from_raw(dw, dh, dst_raw).expect("buffer dimensions match");
    (img, sx_ratio, sy_ratio)
}

/// Estima el color de la tapa blanca del escáner usando el percentil 90 de los bordes.
fn estimate_background_rgb(img: &RgbImage) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let mut rs = Vec::new();
    let mut gs = Vec::new();
    let mut bs = Vec::new();

    let margin = 4.min(w / 4).min(h / 4).max(1);
    let mut sample = |x: u32, y: u32| {
        let p = img.get_pixel(x, y).0;
        // Ignorar píxeles oscuros si una foto toca el borde del vidrio.
        if p[0] > 200 && p[1] > 200 && p[2] > 200 {
            rs.push(p[0]);
            gs.push(p[1]);
            bs.push(p[2]);
        }
    };

    for y in 0..margin {
        for x in (0..w).step_by(2) {
            sample(x, y);
            sample(x, h - 1 - y);
        }
    }
    for x in 0..margin {
        for y in (0..h).step_by(2) {
            sample(x, y);
            sample(w - 1 - x, y);
        }
    }

    if rs.is_empty() {
        return [253, 253, 253];
    }

    rs.sort_unstable();
    gs.sort_unstable();
    bs.sort_unstable();
    let idx = (rs.len() * 85) / 100;
    [rs[idx], gs[idx], bs[idx]]
}

/// Calcula perfiles 1D separables de fondo (por columna y por fila) para compensar
/// sombras suaves de iluminación en los bordes de la tapa del escáner.
fn estimate_local_background_profiles(
    img: &RgbImage,
    global_bg: [u8; 3],
) -> (Vec<[u8; 3]>, Vec<[u8; 3]>) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let raw = img.as_raw();

    let min_col_samples = 8.max(h / 20);
    let min_row_samples = 8.max(w / 20);

    let mut rs = Vec::with_capacity(w.max(h));
    let mut gs = Vec::with_capacity(w.max(h));
    let mut bs = Vec::with_capacity(w.max(h));

    let mut bg_col = vec![global_bg; w];
    for x in 0..w {
        rs.clear();
        gs.clear();
        bs.clear();
        for y in 0..h {
            let idx = (y * w + x) * 3;
            let r = raw[idx];
            let g = raw[idx + 1];
            let b = raw[idx + 2];
            let min_c = r.min(g).min(b);
            let max_c = r.max(g).max(b);
            if max_c - min_c <= 14 && min_c >= 185 {
                rs.push(r);
                gs.push(g);
                bs.push(b);
            }
        }
        if rs.len() >= min_col_samples {
            rs.sort_unstable();
            gs.sort_unstable();
            bs.sort_unstable();
            let p90 = ((rs.len() - 1) * 90) / 100;
            bg_col[x] = [
                rs[p90].min(global_bg[0]),
                gs[p90].min(global_bg[1]),
                bs[p90].min(global_bg[2]),
            ];
        }
    }

    let mut bg_row = vec![global_bg; h];
    for y in 0..h {
        rs.clear();
        gs.clear();
        bs.clear();
        let row_base = y * w * 3;
        for x in 0..w {
            let idx = row_base + x * 3;
            let r = raw[idx];
            let g = raw[idx + 1];
            let b = raw[idx + 2];
            let min_c = r.min(g).min(b);
            let max_c = r.max(g).max(b);
            if max_c - min_c <= 14 && min_c >= 185 {
                rs.push(r);
                gs.push(g);
                bs.push(b);
            }
        }
        if rs.len() >= min_row_samples {
            rs.sort_unstable();
            gs.sort_unstable();
            bs.sort_unstable();
            let p90 = ((rs.len() - 1) * 90) / 100;
            bg_row[y] = [
                rs[p90].min(global_bg[0]),
                gs[p90].min(global_bg[1]),
                bs[p90].min(global_bg[2]),
            ];
        }
    }

    (smooth_profile_1d(&bg_col, 4), smooth_profile_1d(&bg_row, 4))
}

fn smooth_profile_1d(profile: &[[u8; 3]], radius: usize) -> Vec<[u8; 3]> {
    let n = profile.len();
    if n == 0 || radius == 0 {
        return profile.to_vec();
    }
    let mut out = vec![[0u8; 3]; n];
    for i in 0..n {
        let start = i.saturating_sub(radius);
        let end = (i + radius).min(n - 1);
        let mut sum = [0u32; 3];
        let count = (end - start + 1) as u32;
        for p in &profile[start..=end] {
            sum[0] += p[0] as u32;
            sum[1] += p[1] as u32;
            sum[2] += p[2] as u32;
        }
        out[i] = [
            (sum[0] / count) as u8,
            (sum[1] / count) as u8,
            (sum[2] / count) as u8,
        ];
    }
    out
}

fn erode_3x3(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h];
    for y in 1..h - 1 {
        let row = y * w;
        for x in 1..w - 1 {
            let i = row + x;
            if src[i] == 1
                && src[i - 1] == 1
                && src[i + 1] == 1
                && src[i - w] == 1
                && src[i + w] == 1
                && src[i - w - 1] == 1
                && src[i - w + 1] == 1
                && src[i + w - 1] == 1
                && src[i + w + 1] == 1
            {
                dst[i] = 1;
            }
        }
    }
    dst
}

fn dilate_3x3(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h];
    for y in 1..h - 1 {
        let row = y * w;
        for x in 1..w - 1 {
            let i = row + x;
            if src[i] == 1
                || src[i - 1] == 1
                || src[i + 1] == 1
                || src[i - w] == 1
                || src[i + w] == 1
                || src[i - w - 1] == 1
                || src[i - w + 1] == 1
                || src[i + w - 1] == 1
                || src[i + w + 1] == 1
            {
                dst[i] = 1;
            }
        }
    }
    dst
}

/// Rellena cualquier hueco de fondo que no sea alcanzable desde el borde exterior.
fn fill_internal_holes(mask: &mut [u8], w: usize, h: usize) {
    let mut visited = vec![0u8; w * h];
    let mut stack = Vec::with_capacity(w * 2 + h * 2);

    // Sembrar desde (0, 0), garantizado en el padding sintético.
    if mask[0] == 0 {
        visited[0] = 1;
        stack.push(0usize);
    }

    while let Some(idx) = stack.pop() {
        let x = idx % w;
        let y = idx / w;
        if x > 0 {
            let n = idx - 1;
            if mask[n] == 0 && visited[n] == 0 {
                visited[n] = 1;
                stack.push(n);
            }
        }
        if x + 1 < w {
            let n = idx + 1;
            if mask[n] == 0 && visited[n] == 0 {
                visited[n] = 1;
                stack.push(n);
            }
        }
        if y > 0 {
            let n = idx - w;
            if mask[n] == 0 && visited[n] == 0 {
                visited[n] = 1;
                stack.push(n);
            }
        }
        if y + 1 < h {
            let n = idx + w;
            if mask[n] == 0 && visited[n] == 0 {
                visited[n] = 1;
                stack.push(n);
            }
        }
    }

    for i in 0..mask.len() {
        if mask[i] == 0 && visited[i] == 0 {
            mask[i] = 1;
        }
    }
}

fn extract_components(mask: &[u8], w: usize, h: usize) -> Vec<Vec<usize>> {
    let mut seen = vec![0u8; w * h];
    let mut components = Vec::new();
    let mut stack = Vec::new();

    for i in 0..mask.len() {
        if mask[i] == 0 || seen[i] == 1 {
            continue;
        }
        seen[i] = 1;
        stack.push(i);
        let mut comp = Vec::new();
        while let Some(idx) = stack.pop() {
            comp.push(idx);
            let x = idx % w;
            let y = idx / w;
            if x > 0 {
                let n = idx - 1;
                if mask[n] == 1 && seen[n] == 0 {
                    seen[n] = 1;
                    stack.push(n);
                }
            }
            if x + 1 < w {
                let n = idx + 1;
                if mask[n] == 1 && seen[n] == 0 {
                    seen[n] = 1;
                    stack.push(n);
                }
            }
            if y > 0 {
                let n = idx - w;
                if mask[n] == 1 && seen[n] == 0 {
                    seen[n] = 1;
                    stack.push(n);
                }
            }
            if y + 1 < h {
                let n = idx + w;
                if mask[n] == 1 && seen[n] == 0 {
                    seen[n] = 1;
                    stack.push(n);
                }
            }
        }
        components.push(comp);
    }

    components
}

fn component_boundary_points(comp: &[usize], mask: &[u8], w: usize, h: usize) -> Vec<(f32, f32)> {
    let mut pts = Vec::new();
    for &idx in comp {
        let x = idx % w;
        let y = idx / w;
        let on_edge = x == 0
            || y == 0
            || x + 1 == w
            || y + 1 == h
            || mask[idx - 1] == 0
            || mask[idx + 1] == 0
            || mask[idx - w] == 0
            || mask[idx + w] == 0;
        if on_edge {
            pts.push((x as f32, y as f32));
        }
    }
    pts
}

/// Envolvente convexa (algoritmo de cadena monótona de Andrew).
fn convex_hull(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pts.dedup();
    if pts.len() <= 2 {
        return pts;
    }

    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| -> f32 {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };

    let mut lower: Vec<(f32, f32)> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }

    let mut upper: Vec<(f32, f32)> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }

    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Rectángulo orientado de área mínima sobre una envolvente convexa.
fn min_area_bounding_rect(hull: &[(f32, f32)]) -> Option<OrientedRect> {
    if hull.len() < 3 {
        return None;
    }

    let mut best_area = f32::INFINITY;
    let mut best: Option<OrientedRect> = None;

    for i in 0..hull.len() {
        let p0 = hull[i];
        let p1 = hull[(i + 1) % hull.len()];
        let dx = p1.0 - p0.0;
        let dy = p1.1 - p0.1;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            continue;
        }
        let ux = dx / len;
        let uy = dy / len;
        let vx = -uy;
        let vy = ux;

        let mut min_u = f32::INFINITY;
        let mut max_u = f32::NEG_INFINITY;
        let mut min_v = f32::INFINITY;
        let mut max_v = f32::NEG_INFINITY;

        for &(px, py) in hull {
            let u = px * ux + py * uy;
            let v = px * vx + py * vy;
            min_u = min_u.min(u);
            max_u = max_u.max(u);
            min_v = min_v.min(v);
            max_v = max_v.max(v);
        }

        let w = max_u - min_u;
        let h = max_v - min_v;
        let area = w * h;
        if area < best_area {
            best_area = area;
            let cu = (min_u + max_u) * 0.5;
            let cv = (min_v + max_v) * 0.5;
            let cx = cu * ux + cv * vx;
            let cy = cu * uy + cv * vy;
            let angle_deg = uy.atan2(ux).to_degrees();
            best = Some(OrientedRect {
                cx,
                cy,
                width: w,
                height: h,
                angle_deg,
            });
        }
    }

    // Normalizar para que angle_deg esté cerca de 0° (-45°..45°), intercambiando w/h si hace falta.
    best.map(|mut r| {
        let mut a = r.angle_deg.rem_euclid(360.0);
        if a > 180.0 {
            a -= 360.0;
        }
        while a > 45.0 {
            a -= 90.0;
            std::mem::swap(&mut r.width, &mut r.height);
        }
        while a < -45.0 {
            a += 90.0;
            std::mem::swap(&mut r.width, &mut r.height);
        }
        r.angle_deg = a;
        r
    })
}

fn normalize_deskew_angle(angle_deg: f32) -> f32 {
    if angle_deg.abs() < 0.08 {
        0.0
    } else {
        angle_deg
    }
}

fn rect_aabb(r: OrientedRect) -> (f32, f32, f32, f32) {
    let corners = r.corners();
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    (min_x, min_y, max_x, max_y)
}

/// Defensa en profundidad: descarta artefactos gigantes cuyo AABB envuelve >85%
/// de otra foto detectada y tiene más del doble de área.
fn drop_wrapping_artifacts(rects: Vec<OrientedRect>) -> Vec<OrientedRect> {
    if rects.len() <= 1 {
        return rects;
    }
    let boxes: Vec<(f32, f32, f32, f32)> = rects.iter().copied().map(rect_aabb).collect();
    let areas: Vec<f32> = boxes
        .iter()
        .map(|&(x0, y0, x1, y1)| (x1 - x0).max(0.0) * (y1 - y0).max(0.0))
        .collect();

    let mut keep = vec![true; rects.len()];
    for i in 0..rects.len() {
        let (ax0, ay0, ax1, ay1) = boxes[i];
        let area_i = areas[i];
        for j in 0..rects.len() {
            if i == j {
                continue;
            }
            let area_j = areas[j];
            if area_j <= 0.0 || area_i <= area_j * 2.0 {
                continue;
            }
            let (bx0, by0, bx1, by1) = boxes[j];
            let iw = (ax1.min(bx1) - ax0.max(bx0)).max(0.0);
            let ih = (ay1.min(by1) - ay0.max(by0)).max(0.0);
            let containment = (iw * ih) / area_j;
            if containment > 0.85 {
                keep[i] = false;
                break;
            }
        }
    }

    rects
        .into_iter()
        .zip(keep)
        .filter_map(|(r, k)| k.then_some(r))
        .collect()
}

const STRIP_MAX_DIM: f32 = 520.0;
const STRIP_PAD: usize = 12;

struct InspectionStrip {
    raw: Vec<u8>,
    w: usize,
    h: usize,
    inner_w: usize,
    inner_h: usize,
    scale: f32,
}

impl InspectionStrip {
    #[inline]
    fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.w + x) * 3;
        [self.raw[i], self.raw[i + 1], self.raw[i + 2]]
    }
}

/// Etapa 2: refina ángulo fino (Theil-Sen) y recorta bordes blancos o sombras de papel
/// curvado en los 4 lados de forma independiente sobre el escaneo original.
pub fn refine_oriented_rect(scan: &RgbImage, coarse: OrientedRect) -> OrientedRect {
    refine_oriented_rect_with_inset(scan, coarse, 6.0)
}

fn refine_oriented_rect_with_inset(
    scan: &RgbImage,
    coarse: OrientedRect,
    base_inset_px: f32,
) -> OrientedRect {
    if coarse.width < 24.0 || coarse.height < 24.0 {
        return coarse;
    }

    let mut rect = coarse;
    let mut strip = sample_inspection_strip(scan, rect);

    // Paso 2a: refinamiento robusto de ángulo fino (mediana de pendientes Theil-Sen).
    if let Some(delta_deg) = estimate_angle_delta(&strip)
        && delta_deg.abs() >= 0.12
    {
        rect.angle_deg = normalize_deskew_angle(rect.angle_deg + delta_deg);
        strip = sample_inspection_strip(scan, rect);
    }

    // Paso 2b: recorte independiente por lado de sombras externas y bordes blancos.
    let trim_l = trim_side_inward(&strip, Side::Left);
    let trim_r = trim_side_inward(&strip, Side::Right);
    let trim_t = trim_side_inward(&strip, Side::Top);
    let trim_b = trim_side_inward(&strip, Side::Bottom);

    let to_orig = |trim_strip: usize, side_orig: f32| -> f32 {
        if trim_strip > 0 {
            ((trim_strip as f32 + 1.0) / strip.scale).min(side_orig * 0.075)
        } else {
            base_inset_px.min(side_orig * 0.05)
        }
    };

    let tl = to_orig(trim_l, rect.width);
    let tr = to_orig(trim_r, rect.width);
    let tt = to_orig(trim_t, rect.height);
    let tb = to_orig(trim_b, rect.height);

    let new_w = (rect.width - tl - tr).max(16.0);
    let new_h = (rect.height - tt - tb).max(16.0);

    let dlx = (tl - tr) * 0.5;
    let dly = (tt - tb) * 0.5;
    let (sin_a, cos_a) = rect.angle_deg.to_radians().sin_cos();

    let (orig_w, orig_h) = scan.dimensions();
    OrientedRect {
        cx: (rect.cx + dlx * cos_a - dly * sin_a).clamp(0.0, orig_w as f32),
        cy: (rect.cy + dlx * sin_a + dly * cos_a).clamp(0.0, orig_h as f32),
        width: new_w,
        height: new_h,
        angle_deg: rect.angle_deg,
    }
}

fn sample_inspection_strip(scan: &RgbImage, rect: OrientedRect) -> InspectionStrip {
    let max_side = rect.width.max(rect.height).max(1.0);
    let scale = (STRIP_MAX_DIM / max_side).min(1.0);
    let inner_w = (rect.width * scale).round().max(16.0) as usize;
    let inner_h = (rect.height * scale).round().max(16.0) as usize;
    let w = inner_w + STRIP_PAD * 2;
    let h = inner_h + STRIP_PAD * 2;

    let (sin_a, cos_a) = rect.angle_deg.to_radians().sin_cos();
    let inv_scale = 1.0 / scale;
    let cx_s = STRIP_PAD as f32 + (inner_w as f32 - 1.0) * 0.5;
    let cy_s = STRIP_PAD as f32 + (inner_h as f32 - 1.0) * 0.5;

    let (sw, sh) = scan.dimensions();
    let sw_f = (sw - 1) as f32;
    let sh_f = (sh - 1) as f32;
    let sw_u = sw as usize;
    let src = scan.as_raw();

    let mut raw = vec![254u8; w * h * 3];
    for y in 0..h {
        let ly = (y as f32 - cy_s) * inv_scale;
        let dst_row = y * w * 3;
        for x in 0..w {
            let lx = (x as f32 - cx_s) * inv_scale;
            let sx = rect.cx + lx * cos_a - ly * sin_a;
            let sy = rect.cy + lx * sin_a + ly * cos_a;

            if sx < -0.5 || sy < -0.5 || sx > sw_f + 0.5 || sy > sh_f + 0.5 {
                continue;
            }

            let sx_c = sx.clamp(0.0, sw_f);
            let sy_c = sy.clamp(0.0, sh_f);
            let x0 = sx_c.floor() as usize;
            let y0 = sy_c.floor() as usize;
            let x1 = (x0 + 1).min(sw_u - 1);
            let y1 = (y0 + 1).min(sh as usize - 1);
            let fx = sx_c - x0 as f32;
            let fy = sy_c - y0 as f32;

            let i00 = (y0 * sw_u + x0) * 3;
            let i10 = (y0 * sw_u + x1) * 3;
            let i01 = (y1 * sw_u + x0) * 3;
            let i11 = (y1 * sw_u + x1) * 3;

            let dst = dst_row + x * 3;
            for c in 0..3 {
                let top = src[i00 + c] as f32 + (src[i10 + c] as f32 - src[i00 + c] as f32) * fx;
                let bot = src[i01 + c] as f32 + (src[i11 + c] as f32 - src[i01 + c] as f32) * fx;
                raw[dst + c] = (top + (bot - top) * fy).clamp(0.0, 255.0) as u8;
            }
        }
    }

    InspectionStrip {
        raw,
        w,
        h,
        inner_w,
        inner_h,
        scale,
    }
}

#[derive(Clone, Copy)]
enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

#[inline]
fn rgb_luma(p: [u8; 3]) -> u8 {
    ((77 * p[0] as u32 + 150 * p[1] as u32 + 29 * p[2] as u32) >> 8) as u8
}

#[inline]
fn rgb_chroma(p: [u8; 3]) -> u8 {
    p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])
}

#[inline]
fn rgb_max_diff(a: [u8; 3], b: [u8; 3]) -> u8 {
    a[0].abs_diff(b[0])
        .max(a[1].abs_diff(b[1]))
        .max(a[2].abs_diff(b[2]))
}

#[inline]
fn is_outer_blank(p: [u8; 3]) -> bool {
    rgb_luma(p) >= 200 && rgb_chroma(p) <= 18
}

fn find_ray_boundary<F: Fn(usize) -> [u8; 3]>(max_d: usize, sample: F) -> Option<usize> {
    if max_d < 5 {
        return None;
    }
    // Si el borde exterior ya está dentro de una foto vecina o borde, ignorar este rayo.
    if !is_outer_blank(sample(0)) {
        return None;
    }

    let mut run = 0usize;
    let mut run_start = 0usize;
    for d in 1..max_d {
        let p = sample(d);
        let luma = rgb_luma(p);
        let chroma = rgb_chroma(p);
        let blank = luma >= 200 && chroma <= 18;
        if blank {
            run = 0;
            continue;
        }
        if run == 0 {
            run_start = d;
        }
        run += 1;

        // Interior real: color marcado (>22), oscuro (<145) sostenido 2px, o >=4px no-blancos seguidos.
        if (chroma > 22 && run >= 2) || (luma < 145 && run >= 2) || run >= 4 {
            return Some(run_start);
        }
    }
    None
}

fn estimate_angle_delta(strip: &InspectionStrip) -> Option<f32> {
    const RAYS: usize = 32;
    let mut side_votes: Vec<(f32, usize)> = Vec::with_capacity(4);

    for side in [Side::Top, Side::Bottom, Side::Left, Side::Right] {
        let (along_len, perp_len, strip_perp) = match side {
            Side::Top | Side::Bottom => (strip.inner_w, strip.inner_h, strip.h),
            Side::Left | Side::Right => (strip.inner_h, strip.inner_w, strip.w),
        };
        if along_len < 24 || perp_len < 24 {
            continue;
        }

        let max_d = (STRIP_PAD + ((perp_len as f32) * 0.14).round() as usize).min(strip_perp - 2);
        let u_start = STRIP_PAD as f32 + along_len as f32 * 0.12;
        let u_end = STRIP_PAD as f32 + along_len as f32 * 0.88;
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(RAYS);

        for i in 0..RAYS {
            let t = i as f32 / (RAYS - 1) as f32;
            let u = (u_start + t * (u_end - u_start)).round() as usize;
            let maybe_d = match side {
                Side::Top => find_ray_boundary(max_d, |d| strip.pixel(u, d)),
                Side::Bottom => find_ray_boundary(max_d, |d| strip.pixel(u, strip.h - 1 - d)),
                Side::Left => find_ray_boundary(max_d, |d| strip.pixel(d, u)),
                Side::Right => find_ray_boundary(max_d, |d| strip.pixel(strip.w - 1 - d, u)),
            };

            if let Some(d) = maybe_d {
                let v = match side {
                    Side::Top | Side::Left => d as f32,
                    Side::Bottom => (strip.h - 1 - d) as f32,
                    Side::Right => (strip.w - 1 - d) as f32,
                };
                pts.push((u as f32, v));
            }
        }

        if let Some((slope, inliers)) = theil_sen_fit(&pts) {
            let delta = match side {
                Side::Top | Side::Bottom => slope.atan().to_degrees(),
                Side::Left | Side::Right => -slope.atan().to_degrees(),
            };
            if delta.is_finite() && delta.abs() <= 6.0 {
                side_votes.push((delta, inliers));
            }
        }
    }

    if side_votes.is_empty() {
        return None;
    }

    // Mediana ponderada por número de inliers entre los lados válidos.
    side_votes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total_w: usize = side_votes.iter().map(|v| v.1).sum();
    let half = total_w / 2;
    let mut acc = 0usize;
    let mut chosen = side_votes[0].0;
    for &(angle, w) in &side_votes {
        acc += w;
        if acc >= half {
            chosen = angle;
            break;
        }
    }

    Some(chosen.clamp(-4.0, 4.0))
}

fn theil_sen_fit(pts: &[(f32, f32)]) -> Option<(f32, usize)> {
    if pts.len() < 10 {
        return None;
    }

    let mut slopes = Vec::with_capacity(pts.len() * pts.len() / 2);
    for i in 0..pts.len() {
        for j in (i + 1)..pts.len() {
            let du = pts[j].0 - pts[i].0;
            if du.abs() >= 12.0 {
                slopes.push((pts[j].1 - pts[i].1) / du);
            }
        }
    }
    if slopes.is_empty() {
        return None;
    }
    slopes.sort_by(|a, b| a.total_cmp(b));
    let slope = slopes[slopes.len() / 2];

    let mut intercepts: Vec<f32> = pts.iter().map(|&(u, v)| v - slope * u).collect();
    intercepts.sort_by(|a, b| a.total_cmp(b));
    let intercept = intercepts[intercepts.len() / 2];

    let mut residuals: Vec<f32> = pts
        .iter()
        .map(|&(u, v)| (v - (slope * u + intercept)).abs())
        .collect();
    let inliers = residuals.iter().filter(|&&r| r <= 2.0).count();
    residuals.sort_by(|a, b| a.total_cmp(b));
    let mad = residuals[residuals.len() / 2];

    if inliers >= 10 && mad <= 1.2 {
        Some((slope, inliers))
    } else {
        None
    }
}

fn trim_side_inward(strip: &InspectionStrip, side: Side) -> usize {
    let (span_inner, perp_inner) = match side {
        Side::Top | Side::Bottom => (strip.inner_w, strip.inner_h),
        Side::Left | Side::Right => (strip.inner_h, strip.inner_w),
    };
    let max_trim = ((perp_inner as f32) * 0.065).round() as usize;
    if max_trim == 0 || span_inner < 16 {
        return 0;
    }

    // Muestrear el 88% central del lado para ignorar esquinas redondeadas.
    let margin = ((span_inner as f32) * 0.06).round() as usize;
    let s_start = STRIP_PAD + margin;
    let s_end = (STRIP_PAD + span_inner).saturating_sub(margin);
    if s_end <= s_start + 4 {
        return 0;
    }
    let span_len = s_end - s_start;
    let scan_limit = (max_trim + 4).min(perp_inner / 3).max(max_trim);

    let mut bright_frac = Vec::with_capacity(scan_limit);
    let mut neutral_frac = Vec::with_capacity(scan_limit);
    let mut edge_bright_run_frac = Vec::with_capacity(scan_limit);

    for k in 0..scan_limit {
        let mut bright = 0usize;
        let mut neutral = 0usize;
        let mut left_run = 0usize;
        let mut left_open = true;
        let mut cur_run = 0usize;
        let mut max_run = 0usize;

        let sample_at = |s: usize| -> [u8; 3] {
            match side {
                Side::Top => strip.pixel(s, STRIP_PAD + k),
                Side::Bottom => strip.pixel(s, STRIP_PAD + strip.inner_h - 1 - k),
                Side::Left => strip.pixel(STRIP_PAD + k, s),
                Side::Right => strip.pixel(STRIP_PAD + strip.inner_w - 1 - k, s),
            }
        };

        let mut prev = sample_at(s_start);
        for s in s_start..s_end {
            let p = sample_at(s);
            let edge_diff = if s == s_start {
                rgb_max_diff(p, sample_at((s + 1).min(s_end - 1)))
            } else {
                rgb_max_diff(p, prev)
            };
            prev = p;

            let luma = rgb_luma(p);
            let chroma = rgb_chroma(p);
            let is_bright = luma >= 218 && chroma <= 20 && edge_diff <= 16;
            let is_shadow = luma >= 165 && chroma <= 16 && edge_diff <= 18;

            if is_bright {
                bright += 1;
                cur_run += 1;
                max_run = max_run.max(cur_run);
                if left_open {
                    left_run += 1;
                }
            } else {
                left_open = false;
                cur_run = 0;
            }
            if is_bright || is_shadow {
                neutral += 1;
            }
        }
        let right_run = cur_run;

        let f_len = span_len as f32;
        bright_frac.push(bright as f32 / f_len);
        neutral_frac.push(neutral as f32 / f_len);
        // Racha blanca concentrada en un extremo (cuña de esquina) o tramo continuo dominante.
        let wedge_run = left_run.max(right_run).max(if max_run * 2 >= span_len { max_run } else { 0 });
        edge_bright_run_frac.push(wedge_run as f32 / f_len);
    }

    // Regla principal: avanzar mientras el prefijo sea neutral (>=0.80) o blanco (>=0.78),
    // siempre que exista al menos una fila/columna claramente blanca (>=0.72) en el prefijo.
    let mut trim = 0usize;
    let mut seen_bright = false;
    for k in 0..max_trim {
        if neutral_frac[k] < 0.80 && bright_frac[k] < 0.78 {
            break;
        }
        if bright_frac[k] >= 0.72 {
            seen_bright = true;
        }
        if seen_bright {
            trim = k + 1;
        }
    }

    // Residual de cuña diagonal en esquina: hasta 3px extra solo si termina rápido (<0.50 a +3px),
    // evitando recortar arcos/camisas/cielos blancos que continúan dentro de la foto.
    let lookahead = (trim + 3).min(scan_limit.saturating_sub(1));
    let wedge_terminates = bright_frac.get(lookahead).copied().unwrap_or(0.0) < 0.50;
    if wedge_terminates {
        let extra_limit = (trim + 3).min(max_trim);
        for k in trim..extra_limit {
            if bright_frac[k] >= 0.55 && edge_bright_run_frac[k] >= 0.35 {
                trim = k + 1;
            } else {
                break;
            }
        }
    }

    trim
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn detects_two_photos_including_one_touching_bottom_edge() {
        let mut scan = RgbImage::from_pixel(400, 600, Rgb([253, 253, 253]));

        // Foto 1 arriba-izquierda (140x180)
        for y in 40..220 {
            for x in 30..170 {
                scan.put_pixel(x, y, Rgb([120, 80, 60]));
            }
        }
        // Foto 2 abajo tocando el borde inferior (200x200)
        for y in 400..600 {
            for x in 100..300 {
                scan.put_pixel(x, y, Rgb([60, 110, 160]));
            }
        }

        let photos = detect_photos(&scan, DetectConfig {
            margin_px: 0.0,
            ..Default::default()
        });
        assert_eq!(photos.len(), 2);
        assert!((photos[0].crop.width - 140.0).abs() < 6.0);
        assert!((photos[0].crop.height - 180.0).abs() < 6.0);
        assert!((photos[1].crop.width - 200.0).abs() < 6.0);
        assert!((photos[1].crop.height - 200.0).abs() < 6.0);
    }

    #[test]
    fn left_edge_scanner_shadow_gradient_does_not_stretch_photo() {
        let mut scan = RgbImage::from_pixel(400, 600, Rgb([253, 253, 253]));

        // Gradiente suave de sombra de la tapa del escáner en x=0..15 (215 -> 253).
        for y in 0..600 {
            for x in 0..16 {
                let v = (215.0 + (x as f32 / 15.0) * (253.0 - 215.0)).round() as u8;
                scan.put_pixel(x, y, Rgb([v, v, v]));
            }
        }

        // Foto situada a solo 4px del borde izquierdo (160x180).
        for y in 120..300 {
            for x in 4..164 {
                scan.put_pixel(x, y, Rgb([110, 90, 75]));
            }
        }

        let photos = detect_photos(
            &scan,
            DetectConfig {
                margin_px: 0.0,
                ..Default::default()
            },
        );
        assert_eq!(photos.len(), 1);
        assert!(
            (photos[0].crop.width - 160.0).abs() < 8.0,
            "unexpected width {}",
            photos[0].crop.width
        );
        assert!(
            (photos[0].crop.height - 180.0).abs() < 8.0,
            "photo stretched vertically: height {}",
            photos[0].crop.height
        );
    }

    #[test]
    fn stage2_trims_drop_shadow_and_white_gap_on_bottom_and_right() {
        let mut scan = RgbImage::from_pixel(800, 1000, Rgb([252, 252, 252]));

        // Foto real (500x660) en x=100..600, y=100..760.
        for y in 100..760 {
            for x in 100..600 {
                scan.put_pixel(x, y, Rgb([110, 85, 65]));
            }
        }
        // Sombra fina gris de papel curvado separada por una franja blanca de 25px abajo y a la derecha.
        for y in 785..790 {
            for x in 100..630 {
                scan.put_pixel(x, y, Rgb([195, 195, 195]));
            }
        }
        for y in 100..790 {
            for x in 625..630 {
                scan.put_pixel(x, y, Rgb([195, 195, 195]));
            }
        }

        // Rectángulo grueso (como el hull de Etapa 1 envolviendo foto + hueco blanco + sombra).
        let coarse = OrientedRect {
            cx: 365.0,
            cy: 445.0,
            width: 530.0,
            height: 690.0,
            angle_deg: 0.0,
        };

        let refined = refine_oriented_rect(&scan, coarse);
        assert!(
            (refined.width - 500.0).abs() < 12.0,
            "expected right drop-shadow + 25px gap trimmed (~500), got width {}",
            refined.width
        );
        assert!(
            (refined.height - 660.0).abs() < 12.0,
            "expected bottom drop-shadow + 25px gap trimmed (~660), got height {}",
            refined.height
        );
        let right_edge = refined.cx + refined.width * 0.5;
        let bottom_edge = refined.cy + refined.height * 0.5;
        assert!(
            right_edge <= 601.0 && right_edge >= 590.0,
            "right edge landed at {right_edge}"
        );
        assert!(
            bottom_edge <= 761.0 && bottom_edge >= 750.0,
            "bottom edge landed at {bottom_edge}"
        );
    }

    #[test]
    fn stage2_does_not_clip_white_archway_touching_55_percent_of_edge() {
        let mut scan = RgbImage::from_pixel(800, 1000, Rgb([252, 252, 252]));

        // Foto real (500x660) en x=100..600, y=100..760.
        for y in 100..760 {
            for x in 100..600 {
                scan.put_pixel(x, y, Rgb([60, 110, 155]));
            }
        }
        // Arco / camisa blanca tocando el 55% del borde superior y entrando 150px en la foto.
        for y in 100..250 {
            for x in 100..375 {
                scan.put_pixel(x, y, Rgb([245, 245, 244]));
            }
        }

        let coarse = OrientedRect {
            cx: 350.0,
            cy: 430.0,
            width: 500.0,
            height: 660.0,
            angle_deg: 0.0,
        };

        let refined = refine_oriented_rect(&scan, coarse);
        let top_edge = refined.cy - refined.height * 0.5;
        assert!(
            top_edge <= 110.0,
            "Stage 2 clipped into white archway: top edge moved to {top_edge}"
        );
        assert!(
            refined.height >= 642.0,
            "Stage 2 shrank photo height excessively: {}",
            refined.height
        );
    }
}
