use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use image::imageops::FilterType;
use image::{Rgb, RgbImage};
use photocut_core::detect::{DetectConfig, detect_photos};
use photocut_core::export::{SampleQuality, extract_photo, save_image};
use photocut_core::session::Session;
use photocut_core::{Adjustments, DetectedPhoto, ExportFormat, OrientedRect, QuarterTurn};
use rayon::prelude::*;

const REPRESENTATIVE_SCANS: &[&str] = &[
    "img001.jpg",
    "img003.jpg",
    "img004.jpg",
    "img007.jpg",
    "img008.jpg",
    "img018.jpg",
    "img020.jpg",
    "img024.jpg",
    "img030.jpg",
    "img035.jpg",
    "img037.jpg",
    "img040.jpg",
    "img041.jpg",
];

const SPOTLIGHT_SCANS: &[&str] = &[
    "img001.jpg",
    "img007.jpg",
    "img008.jpg",
    "img018.jpg",
    "img024.jpg",
    "img030.jpg",
    "img037.jpg",
    "img040.jpg",
    "img041.jpg",
];

struct ScanReport {
    rel_path: String,
    scan_dims: (u32, u32),
    decode_ms: f64,
    detect_ms: f64,
    photos: Vec<DetectedPhoto>,
    after_edge_white: Vec<[f32; 4]>,
    before_edge_white: Vec<[f32; 4]>,
    fullres: Option<FullResReport>,
    error: Option<String>,
}

struct FullResReport {
    dims: (u32, u32),
    extract_ms: f64,
    png_path: PathBuf,
    png_ms: f64,
    png_bytes: u64,
    jpg_path: PathBuf,
    jpg_ms: f64,
    jpg_bytes: u64,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let input_dir = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("data-photo"));
    let output_dir = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("tmp/verify"));
    let before_map: HashMap<String, Vec<OrientedRect>> = args
        .get(3)
        .map(|p| {
            let sess = Session::load(Path::new(p))
                .unwrap_or_else(|e| panic!("failed to load before session {p}: {e}"));
            sess.scans
                .into_iter()
                .map(|s| {
                    let rects = s.photos.into_iter().map(|ph| ph.crop).collect();
                    (s.rel_path.replace('\\', "/"), rects)
                })
                .collect()
        })
        .unwrap_or_default();

    let mut files = Vec::new();
    collect_images(&input_dir, &mut files);
    files.sort();

    if files.is_empty() {
        eprintln!("No image files found under {}", input_dir.display());
        std::process::exit(1);
    }

    let overlays_dir = output_dir.join("overlays");
    let color_samples_dir = output_dir.join("color_samples");
    let fullres_dir = output_dir.join("fullres_sample");
    fs::create_dir_all(&overlays_dir).expect("create overlays dir");
    fs::create_dir_all(&color_samples_dir).expect("create color_samples dir");
    fs::create_dir_all(&fullres_dir).expect("create fullres_sample dir");

    println!(
        "Processing {} scans from {} -> {}",
        files.len(),
        input_dir.display(),
        output_dir.display()
    );

    let wall_start = Instant::now();

    let reports: Vec<ScanReport> = files
        .par_iter()
        .map(|path| {
            process_scan(
                path,
                &input_dir,
                &overlays_dir,
                &color_samples_dir,
                &fullres_dir,
                &before_map,
            )
        })
        .collect();

    let total_wall_ms = wall_start.elapsed().as_secs_f64() * 1000.0;

    print_summary(&reports, total_wall_ms, &output_dir);
}

fn collect_images(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_images(&path, out);
        } else if is_supported_image(&path) {
            out.push(path);
        }
    }
}

fn is_supported_image(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff"
    )
}

fn process_scan(
    path: &Path,
    input_dir: &Path,
    overlays_dir: &Path,
    color_samples_dir: &Path,
    fullres_dir: &Path,
    before_map: &HashMap<String, Vec<OrientedRect>>,
) -> ScanReport {
    let rel_path_buf = path.strip_prefix(input_dir).unwrap_or(path).to_path_buf();
    let rel_path = rel_path_buf.to_string_lossy().replace('\\', "/");

    let t_decode = Instant::now();
    let img = match image::open(path) {
        Ok(dyn_img) => dyn_img.to_rgb8(),
        Err(err) => {
            return ScanReport {
                rel_path,
                scan_dims: (0, 0),
                decode_ms: t_decode.elapsed().as_secs_f64() * 1000.0,
                detect_ms: 0.0,
                photos: Vec::new(),
                after_edge_white: Vec::new(),
                before_edge_white: Vec::new(),
                fullres: None,
                error: Some(err.to_string()),
            };
        }
    };
    let decode_ms = t_decode.elapsed().as_secs_f64() * 1000.0;
    let scan_dims = img.dimensions();

    let t_detect = Instant::now();
    let photos = detect_photos(&img, DetectConfig::default());
    let detect_ms = t_detect.elapsed().as_secs_f64() * 1000.0;

    let after_edge_white: Vec<[f32; 4]> = photos
        .iter()
        .map(|p| measure_crop_edge_white(&img, p.crop))
        .collect();
    let before_edge_white: Vec<[f32; 4]> = before_map
        .get(&rel_path)
        .map(|rects| {
            rects
                .iter()
                .map(|&r| measure_crop_edge_white(&img, r))
                .collect()
        })
        .unwrap_or_default();

    // Save visual diagnostic overlay (~1000px max side)
    let overlay_name = format!(
        "{}.jpg",
        rel_path_buf
            .with_extension("")
            .to_string_lossy()
            .replace(['/', '\\'], "_")
    );
    let overlay_path = overlays_dir.join(overlay_name);
    let overlay = render_overlay(&img, &photos, 1000);
    if let Err(err) = save_image(&overlay, &overlay_path, ExportFormat::Jpeg { quality: 90 }) {
        eprintln!("Failed to save overlay {}: {err}", overlay_path.display());
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let file_stem = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("scan");

    // Save side-by-side color samples for representative scans
    if REPRESENTATIVE_SCANS
        .iter()
        .any(|&rep| rep.eq_ignore_ascii_case(file_name))
    {
        for photo in &photos {
            let orig_adj = Adjustments {
                auto_color_strength: 0.0,
                ..Default::default()
            };
            let enh_adj = Adjustments {
                auto_color_strength: 0.7,
                ..Default::default()
            };
            let orig = extract_photo(
                &img,
                photo.crop,
                photo.turn,
                orig_adj,
                SampleQuality::Bicubic,
                Some(700),
            );
            let enh = extract_photo(
                &img,
                photo.crop,
                photo.turn,
                enh_adj,
                SampleQuality::Bicubic,
                Some(700),
            );
            let combined = side_by_side(&orig, &enh);
            let sample_path = color_samples_dir.join(format!("{file_stem}_photo{}.jpg", photo.id));
            if let Err(err) = save_image(&combined, &sample_path, ExportFormat::Jpeg { quality: 92 })
            {
                eprintln!("Failed to save color sample {}: {err}", sample_path.display());
            }
        }
    }

    // Full-resolution Bicubic export benchmark for first photo of img001.jpg
    let fullres = if file_name.eq_ignore_ascii_case("img001.jpg") {
        photos.first().map(|photo| {
            let t_ext = Instant::now();
            let extracted = extract_photo(
                &img,
                photo.crop,
                photo.turn,
                Adjustments::default(),
                SampleQuality::Bicubic,
                None,
            );
            let extract_ms = t_ext.elapsed().as_secs_f64() * 1000.0;
            let dims = extracted.dimensions();

            let png_path = fullres_dir.join(format!("{file_stem}_photo{}.png", photo.id));
            let t_png = Instant::now();
            save_image(&extracted, &png_path, ExportFormat::Png).expect("save fullres png");
            let png_ms = t_png.elapsed().as_secs_f64() * 1000.0;
            let png_bytes = fs::metadata(&png_path).map(|m| m.len()).unwrap_or(0);

            let jpg_path = fullres_dir.join(format!("{file_stem}_photo{}.jpg", photo.id));
            let t_jpg = Instant::now();
            save_image(&extracted, &jpg_path, ExportFormat::Jpeg { quality: 98 })
                .expect("save fullres jpg");
            let jpg_ms = t_jpg.elapsed().as_secs_f64() * 1000.0;
            let jpg_bytes = fs::metadata(&jpg_path).map(|m| m.len()).unwrap_or(0);

            FullResReport {
                dims,
                extract_ms,
                png_path,
                png_ms,
                png_bytes,
                jpg_path,
                jpg_ms,
                jpg_bytes,
            }
        })
    } else {
        None
    };

    ScanReport {
        rel_path,
        scan_dims,
        decode_ms,
        detect_ms,
        photos,
        after_edge_white,
        before_edge_white,
        fullres,
        error: None,
    }
}

/// Mide la fracción de píxeles blanco-neutro en la franja exterior (3px) de los 4 lados [T, B, L, R]
/// sobre una extracción rápida de ~520px (sin ajustes de color).
fn measure_crop_edge_white(scan: &RgbImage, crop: OrientedRect) -> [f32; 4] {
    let img = extract_photo(
        scan,
        crop,
        QuarterTurn::Deg0,
        Adjustments {
            auto_color_strength: 0.0,
            ..Default::default()
        },
        SampleQuality::Bilinear,
        Some(520),
    );
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w < 16 || h < 16 {
        return [0.0; 4];
    }
    let raw = img.as_raw();
    let is_white_px = |x: usize, y: usize| -> bool {
        let i = (y * w + x) * 3;
        let (r, g, b) = (raw[i], raw[i + 1], raw[i + 2]);
        let luma = (77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8;
        let chroma = r.max(g).max(b) - r.min(g).min(b);
        luma >= 218 && chroma <= 20
    };

    let mx = (w as f32 * 0.06).round() as usize;
    let my = (h as f32 * 0.06).round() as usize;
    let rim = 3usize;

    let mut top = 0usize;
    let mut bot = 0usize;
    let x_cnt = (w - 2 * mx) * rim;
    for k in 0..rim {
        for x in mx..(w - mx) {
            if is_white_px(x, k) {
                top += 1;
            }
            if is_white_px(x, h - 1 - k) {
                bot += 1;
            }
        }
    }

    let mut left = 0usize;
    let mut right = 0usize;
    let y_cnt = (h - 2 * my) * rim;
    for k in 0..rim {
        for y in my..(h - my) {
            if is_white_px(k, y) {
                left += 1;
            }
            if is_white_px(w - 1 - k, y) {
                right += 1;
            }
        }
    }

    [
        top as f32 / x_cnt.max(1) as f32,
        bot as f32 / x_cnt.max(1) as f32,
        left as f32 / y_cnt.max(1) as f32,
        right as f32 / y_cnt.max(1) as f32,
    ]
}

fn render_overlay(scan: &RgbImage, photos: &[DetectedPhoto], max_dim: u32) -> RgbImage {
    let (sw, sh) = scan.dimensions();
    let longest = sw.max(sh);
    let scale = if longest > max_dim {
        max_dim as f32 / longest as f32
    } else {
        1.0
    };
    let ow = ((sw as f32 * scale).round() as u32).max(1);
    let oh = ((sh as f32 * scale).round() as u32).max(1);

    let mut out = image::imageops::resize(scan, ow, oh, FilterType::Triangle);
    let sx = ow as f32 / sw as f32;
    let sy = oh as f32 / sh as f32;

    let green = Rgb([0, 255, 0]);
    let red = Rgb([255, 0, 0]);

    for photo in photos {
        let corners = photo.crop.corners().map(|(x, y)| (x * sx, y * sy));
        for i in 0..4 {
            let p0 = corners[i];
            let p1 = corners[(i + 1) % 4];
            draw_thick_line(&mut out, p0, p1, 3, green);
        }
        for &(cx, cy) in &corners {
            draw_filled_circle(&mut out, cx, cy, 5, red);
        }
    }

    out
}

fn draw_thick_line(img: &mut RgbImage, p0: (f32, f32), p1: (f32, f32), thickness: i32, color: Rgb<u8>) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let dx = p1.0 - p0.0;
    let dy = p1.1 - p0.1;
    let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as i32;
    let half = thickness / 2;

    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let x = (p0.0 + dx * t).round() as i32;
        let y = (p0.1 + dy * t).round() as i32;
        for oy in -half..=half {
            let py = y + oy;
            if py < 0 || py >= h {
                continue;
            }
            for ox in -half..=half {
                let px = x + ox;
                if px >= 0 && px < w {
                    img.put_pixel(px as u32, py as u32, color);
                }
            }
        }
    }
}

fn draw_filled_circle(img: &mut RgbImage, cx: f32, cy: f32, radius: i32, color: Rgb<u8>) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let ix = cx.round() as i32;
    let iy = cy.round() as i32;
    let r2 = radius * radius;
    for oy in -radius..=radius {
        let py = iy + oy;
        if py < 0 || py >= h {
            continue;
        }
        for ox in -radius..=radius {
            if ox * ox + oy * oy <= r2 {
                let px = ix + ox;
                if px >= 0 && px < w {
                    img.put_pixel(px as u32, py as u32, color);
                }
            }
        }
    }
}

fn side_by_side(left: &RgbImage, right: &RgbImage) -> RgbImage {
    const GAP: u32 = 4;
    let w = left.width() + GAP + right.width();
    let h = left.height().max(right.height());
    let mut out = RgbImage::from_pixel(w, h, Rgb([40, 40, 40]));

    for y in 0..left.height() {
        for x in 0..left.width() {
            out.put_pixel(x, y, *left.get_pixel(x, y));
        }
    }
    let offset = left.width() + GAP;
    for y in 0..right.height() {
        for x in 0..right.width() {
            out.put_pixel(offset + x, y, *right.get_pixel(x, y));
        }
    }
    out
}

fn print_summary(reports: &[ScanReport], total_wall_ms: f64, output_dir: &Path) {
    println!();
    println!(
        "{:<24} | {:>3} | {:<11} | {:<32} | {:<24} | {:>8} | {:>8}",
        "Scan", "Cnt", "Scan Dims", "Crop Dims (WxH)", "Angles (deg)", "Dec ms", "Det ms"
    );
    println!("{}", "-".repeat(126));

    let mut total_photos = 0usize;
    let mut sum_decode = 0.0f64;
    let mut sum_detect = 0.0f64;
    let mut anomalies: Vec<String> = Vec::new();
    let mut fullres_report: Option<&FullResReport> = None;

    for r in reports {
        if let Some(err) = &r.error {
            println!("{:<24} | ERR | {}", r.rel_path, err);
            anomalies.push(format!("{}: decode error ({err})", r.rel_path));
            continue;
        }

        total_photos += r.photos.len();
        sum_decode += r.decode_ms;
        sum_detect += r.detect_ms;

        if r.fullres.is_some() {
            fullres_report = r.fullres.as_ref();
        }

        if r.photos.is_empty() {
            anomalies.push(format!("{}: count == 0 (no photos detected)", r.rel_path));
        }

        let dims_str = r
            .photos
            .iter()
            .map(|p| format!("{}x{}", p.crop.width.round() as u32, p.crop.height.round() as u32))
            .collect::<Vec<_>>()
            .join(", ");

        let angles_str = r
            .photos
            .iter()
            .map(|p| format!("{:+.2}", p.crop.angle_deg))
            .collect::<Vec<_>>()
            .join(", ");

        for p in &r.photos {
            let ratio = p.crop.width / p.crop.height.max(1.0);
            if p.crop.angle_deg.abs() > 8.0 {
                anomalies.push(format!(
                    "{} photo#{}: large angle {:+.2} deg (crop {}x{})",
                    r.rel_path,
                    p.id,
                    p.crop.angle_deg,
                    p.crop.width.round() as u32,
                    p.crop.height.round() as u32
                ));
            }
            if ratio > 2.5 || ratio < 0.4 {
                anomalies.push(format!(
                    "{} photo#{}: extreme aspect ratio {:.2} (crop {}x{})",
                    r.rel_path,
                    p.id,
                    ratio,
                    p.crop.width.round() as u32,
                    p.crop.height.round() as u32
                ));
            }
            if p.crop.width >= r.scan_dims.0 as f32 * 0.98
                || p.crop.height >= r.scan_dims.1 as f32 * 0.98
            {
                anomalies.push(format!(
                    "{} photo#{}: spans >=98% of scan edge (crop {}x{} on {}x{})",
                    r.rel_path,
                    p.id,
                    p.crop.width.round() as u32,
                    p.crop.height.round() as u32,
                    r.scan_dims.0,
                    r.scan_dims.1
                ));
            }
        }

        println!(
            "{:<24} | {:>3} | {:>4}x{:<6} | {:<32} | {:<24} | {:>8.1} | {:>8.1}",
            r.rel_path,
            r.photos.len(),
            r.scan_dims.0,
            r.scan_dims.1,
            dims_str,
            angles_str,
            r.decode_ms,
            r.detect_ms
        );
    }

    println!("{}", "-".repeat(126));
    let n = reports.len().max(1) as f64;
    println!(
        "Total scans: {} | Total detected photos: {} | Total wall time: {:.2} s ({:.1} ms)",
        reports.len(),
        total_photos,
        total_wall_ms / 1000.0,
        total_wall_ms
    );
    println!(
        "Per-scan avg (parallel): decode {:.1} ms | detect {:.1} ms | sum decode {:.1} ms | sum detect {:.1} ms",
        sum_decode / n,
        sum_detect / n,
        sum_decode,
        sum_detect
    );

    if let Some(fr) = fullres_report {
        println!();
        println!("=== Full-Res Bicubic Export Sample (img001 photo#0) ===");
        println!(
            "Dimensions: {}x{} | Extract (Bicubic + auto_color=0.7): {:.1} ms",
            fr.dims.0, fr.dims.1, fr.extract_ms
        );
        println!(
            "PNG  -> {} | {:.1} ms | {:.2} MB ({} bytes)",
            fr.png_path.display(),
            fr.png_ms,
            fr.png_bytes as f64 / (1024.0 * 1024.0),
            fr.png_bytes
        );
        println!(
            "JPEG -> {} | {:.1} ms | {:.2} MB ({} bytes)",
            fr.jpg_path.display(),
            fr.jpg_ms,
            fr.jpg_bytes as f64 / (1024.0 * 1024.0),
            fr.jpg_bytes
        );
    }

    println!();
    println!("=== Suspicious Detections / Anomalies ({}) ===", anomalies.len());
    if anomalies.is_empty() {
        println!("None.");
    } else {
        for a in &anomalies {
            println!("  [!] {a}");
        }
    }

    print_white_border_metrics(reports);

    println!();
    println!("Diagnostic outputs:");
    println!("  Overlays:      {}", output_dir.join("overlays").display());
    println!("  Color samples: {}", output_dir.join("color_samples").display());
    println!("  Fullres:       {}", output_dir.join("fullres_sample").display());
}

fn print_white_border_metrics(reports: &[ScanReport]) {
    let summarize = |all: &[&[f32; 4]]| -> (usize, usize, usize, f32) {
        let mut photos_with_white_70 = 0usize;
        let mut edges_white_70 = 0usize;
        let mut edges_white_50 = 0usize;
        let mut sum_edge = 0.0f32;
        for sides in all {
            let mut has_70 = false;
            for &s in sides.iter() {
                sum_edge += s;
                if s >= 0.70 {
                    edges_white_70 += 1;
                    has_70 = true;
                }
                if s >= 0.50 {
                    edges_white_50 += 1;
                }
            }
            if has_70 {
                photos_with_white_70 += 1;
            }
        }
        let mean = sum_edge / (all.len().max(1) * 4) as f32;
        (photos_with_white_70, edges_white_70, edges_white_50, mean)
    };

    let after_all: Vec<&[f32; 4]> = reports
        .iter()
        .flat_map(|r| r.after_edge_white.iter())
        .collect();
    let before_all: Vec<&[f32; 4]> = reports
        .iter()
        .flat_map(|r| r.before_edge_white.iter())
        .collect();

    let (ap70, ae70, ae50, amean) = summarize(&after_all);
    println!();
    println!("=== White-Border Rim Metrics (outer 3px across {} photos / {} edges) ===", after_all.len(), after_all.len() * 4);
    if !before_all.is_empty() {
        let (bp70, be70, be50, bmean) = summarize(&before_all);
        println!(
            "  BEFORE (Stage 1): photos w/ white edge (>=70%): {:>3}/{} | edges >=70%: {:>3} | edges >=50%: {:>3} | mean edge white: {:.2}%",
            bp70,
            before_all.len(),
            be70,
            be50,
            bmean * 100.0
        );
    }
    println!(
        "  AFTER  (Stage 2): photos w/ white edge (>=70%): {:>3}/{} | edges >=70%: {:>3} | edges >=50%: {:>3} | mean edge white: {:.2}%",
        ap70,
        after_all.len(),
        ae70,
        ae50,
        amean * 100.0
    );

    if !before_all.is_empty() {
        println!();
        println!("  Spotlight Scans (max outer-edge white % across [T,B,L,R]: BEFORE -> AFTER):");
        for r in reports {
            let fname = Path::new(&r.rel_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if !SPOTLIGHT_SCANS
                .iter()
                .any(|&s| s.eq_ignore_ascii_case(fname))
            {
                continue;
            }
            let pairs: Vec<String> = r
                .before_edge_white
                .iter()
                .zip(r.after_edge_white.iter())
                .enumerate()
                .map(|(i, (b, a))| {
                    let bm = b.iter().copied().fold(0.0f32, f32::max) * 100.0;
                    let am = a.iter().copied().fold(0.0f32, f32::max) * 100.0;
                    format!("#{i}: {bm:.0}%->{am:.0}%")
                })
                .collect();
            println!("    {:<24} | {}", r.rel_path, pairs.join(", "));
        }
    }
}
