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
        }
        .expanded(cfg.margin_px);

        rects.push(rect);
    }

    // Filtro de seguridad: descartar rectángulos gigantes que envuelven otra foto detectada.
    rects = drop_wrapping_artifacts(rects);

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
}
