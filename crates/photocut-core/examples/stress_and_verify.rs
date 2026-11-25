use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use photocut_core::cache::ScanCache;
use photocut_core::detect::{DetectConfig, detect_photos};
use photocut_core::enhance::apply_adjustments;
use photocut_core::export::{SampleQuality, extract_photo, save_image};
use photocut_core::orient::OrientationModel;
use photocut_core::session::{ScanEntry, Session};
use photocut_core::{Adjustments, ExportFormat, QuarterTurn, ReviewStatus};

const EXPORT_SAMPLE_SCANS: &[&str] = &[
    "img001.jpg",
    "img004.jpg",
    "img007.jpg",
    "img016.jpg",
    "img020.jpg",
    "img035.jpg",
    "img040.jpg",
    "img041.jpg",
];

const AI_MIN_CONFIDENCE: f32 = 0.70;
const AI_CROP_MAX_DIM: u32 = 416;
const PREVIEW_MAX_DIM: u32 = 1400;

#[derive(Debug, Clone, Copy, Default)]
struct MemStats {
    rss_mb: f64,
    hwm_mb: f64,
}

/// Lee VmRSS y VmHWM desde `/proc/self/status` en Linux (fallback seguro en otros SO).
fn read_mem_stats() -> MemStats {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = fs::read_to_string("/proc/self/status") {
            let mut rss_kb = 0.0f64;
            let mut hwm_kb = 0.0f64;
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    if let Some(num) = rest.split_whitespace().next() {
                        rss_kb = num.parse::<f64>().unwrap_or(0.0);
                    }
                } else if let Some(rest) = line.strip_prefix("VmHWM:")
                    && let Some(num) = rest.split_whitespace().next()
                {
                    hwm_kb = num.parse::<f64>().unwrap_or(0.0);
                }
            }
            return MemStats {
                rss_mb: rss_kb / 1024.0,
                hwm_mb: hwm_kb / 1024.0,
            };
        }
    }
    MemStats::default()
}

fn collect_images(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_images(&path, out);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && matches!(
                ext.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "tif" | "tiff"
            )
        {
            out.push(path);
        }
    }
}

fn neutral_adjustments() -> Adjustments {
    Adjustments {
        auto_color_strength: 0.0,
        brightness: 0.0,
        contrast: 0.0,
        saturation: 0.0,
        temperature: 0.0,
        tint: 0.0,
    }
}

struct ExportRecord {
    path: PathBuf,
    format_name: &'static str,
    expected_dims: (u32, u32),
    bytes: u64,
    elapsed_ms: f64,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let input_dir = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("data-photo"));
    let output_dir = PathBuf::from(
        args.get(2)
            .map(String::as_str)
            .unwrap_or("tmp/final_test_export"),
    );

    let mut files = Vec::new();
    collect_images(&input_dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "no scans found under {}",
        input_dir.display()
    );

    let exports_dir = output_dir.join("exports");
    fs::create_dir_all(&exports_dir).expect("create exports dir");

    let t_total = Instant::now();
    let mem_start = read_mem_stats();
    println!(
        "=== PhotoCut Full 41-Scan End-to-End & Memory-Leak Stress Suite ===\nInput: {} ({} scans) | Output: {}\nInitial RSS: {:.1} MB | HWM: {:.1} MB",
        input_dir.display(),
        files.len(),
        output_dir.display(),
        mem_start.rss_mb,
        mem_start.hwm_mb
    );

    // Load ONNX model once (shared across all scans/threads).
    let t_model = Instant::now();
    let ai_model = OrientationModel::find_default().map(|p| {
        println!("Loading ONNX model: {}", p.display());
        OrientationModel::load(&p).expect("load ONNX orientation model")
    });
    if ai_model.is_some() {
        let m = read_mem_stats();
        println!(
            "ONNX model ready in {:.1} ms | RSS: {:.1} MB | HWM: {:.1} MB",
            t_model.elapsed().as_secs_f64() * 1000.0,
            m.rss_mb,
            m.hwm_mb
        );
    } else {
        println!("WARNING: ONNX orientation model not found; running without AI orientation.");
    }

    let mut cache = ScanCache::new(3);
    let mut max_cache_len = 0usize;
    let mut total_photos = 0usize;
    let mut anomalies: Vec<String> = Vec::new();
    let mut turn_counts = [0usize; 4]; // Deg0, Deg90, Deg180, Deg270
    let mut sum_conf = 0.0f64;
    let mut low_conf_count = 0usize;
    let mut ai_infer_wall_ms = 0.0f64;
    let mut scan_entries: Vec<ScanEntry> = Vec::with_capacity(files.len());
    let mut exported_records: Vec<ExportRecord> = Vec::new();
    let mut export_format_idx = 0usize;
    let mut rss_after_warmup = 0.0f64;

    println!();
    println!("--- Phase A: Full 41-Scan Pass (Cache(3) + Detect + ONNX Orient + 1400px Previews + Full-Res Exports) ---");
    let t_phase_a = Instant::now();

    for (idx, path) in files.iter().enumerate() {
        let rel_path = path
            .strip_prefix(&input_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        let scan = cache
            .load_or_get(path)
            .unwrap_or_else(|e| panic!("failed to load {}: {e}", path.display()));
        max_cache_len = max_cache_len.max(cache.len());
        assert!(
            cache.len() <= 3,
            "ScanCache exceeded capacity 3: len={}",
            cache.len()
        );

        let (sw, sh) = scan.dimensions();
        let mut photos = detect_photos(&scan, DetectConfig::default());
        total_photos += photos.len();

        if photos.is_empty() {
            anomalies.push(format!("{rel_path}: count == 0"));
        }
        for p in &photos {
            let ratio = p.crop.width / p.crop.height.max(1.0);
            if p.crop.angle_deg.abs() > 8.0 {
                anomalies.push(format!(
                    "{rel_path} photo#{}: angle {:+.2}",
                    p.id, p.crop.angle_deg
                ));
            }
            if ratio > 2.5 || ratio < 0.4 {
                anomalies.push(format!("{rel_path} photo#{}: aspect {ratio:.2}", p.id));
            }
            if p.crop.width >= sw as f32 * 0.98 || p.crop.height >= sh as f32 * 0.98 {
                anomalies.push(format!(
                    "{rel_path} photo#{}: edge span {}x{}",
                    p.id,
                    p.crop.width.round() as u32,
                    p.crop.height.round() as u32
                ));
            }
        }

        // Sequential ONNX orientation inference (matches worker.rs single-thread arena footprint).
        if let Some(model) = &ai_model {
            let t_ai = Instant::now();
            for photo in &mut photos {
                let crop_ai = extract_photo(
                    &scan,
                    photo.crop,
                    QuarterTurn::Deg0,
                    neutral_adjustments(),
                    SampleQuality::Bilinear,
                    Some(AI_CROP_MAX_DIM),
                );
                let pred = model
                    .predict(&crop_ai, AI_MIN_CONFIDENCE)
                    .expect("ONNX predict");
                photo.turn = pred.turn;
                sum_conf += pred.confidence as f64;
                if pred.confidence < AI_MIN_CONFIDENCE {
                    low_conf_count += 1;
                }
                match pred.turn {
                    QuarterTurn::Deg0 => turn_counts[0] += 1,
                    QuarterTurn::Deg90 => turn_counts[1] += 1,
                    QuarterTurn::Deg180 => turn_counts[2] += 1,
                    QuarterTurn::Deg270 => turn_counts[3] += 1,
                }
            }
            ai_infer_wall_ms += t_ai.elapsed().as_secs_f64() * 1000.0;
        }

        // Generate 1400px preview for every detected photo.
        for photo in &mut photos {
            let preview = extract_photo(
                &scan,
                photo.crop,
                photo.turn,
                photo.adjustments,
                SampleQuality::Bilinear,
                Some(PREVIEW_MAX_DIM),
            );
            assert!(preview.width() > 0 && preview.height() > 0);
            assert!(preview.width().max(preview.height()) <= PREVIEW_MAX_DIM);
            photo.status = ReviewStatus::Approved;
        }

        // Full-res 600 DPI Bicubic exports across all 3 formats for the 8 representative scans.
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("scan");
        if EXPORT_SAMPLE_SCANS
            .iter()
            .any(|&rep| rep.eq_ignore_ascii_case(file_name))
        {
            for photo in &photos {
                let (fmt, fmt_name) = match export_format_idx % 3 {
                    0 => (ExportFormat::Png, "PNG"),
                    1 => (ExportFormat::Tiff, "TIFF"),
                    _ => (ExportFormat::Jpeg { quality: 96 }, "JPEG96"),
                };
                export_format_idx += 1;

                let t_exp = Instant::now();
                let full = extract_photo(
                    &scan,
                    photo.crop,
                    photo.turn,
                    photo.adjustments,
                    SampleQuality::Bicubic,
                    None,
                );
                let expected_dims = full.dimensions();
                let out_path = exports_dir.join(format!(
                    "{file_stem}_photo{}.{}",
                    photo.id,
                    fmt.extension()
                ));
                save_image(&full, &out_path, fmt).expect("save fullres export");
                let elapsed_ms = t_exp.elapsed().as_secs_f64() * 1000.0;
                let bytes = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

                exported_records.push(ExportRecord {
                    path: out_path,
                    format_name: fmt_name,
                    expected_dims,
                    bytes,
                    elapsed_ms,
                });
            }
        }

        scan_entries.push(ScanEntry { rel_path, photos });

        let scan_num = idx + 1;
        if scan_num == 5 {
            rss_after_warmup = read_mem_stats().rss_mb;
        }
        if scan_num == 1 || scan_num % 5 == 0 || scan_num == files.len() {
            let m = read_mem_stats();
            println!(
                "  [Scan {:>2}/{}] cache={}/{} ({:.1} MB raw) | photos_so_far={:>3} | VmRSS={:>6.1} MB | VmHWM={:>6.1} MB",
                scan_num,
                files.len(),
                cache.len(),
                cache.capacity(),
                cache.estimated_bytes() as f64 / (1024.0 * 1024.0),
                total_photos,
                m.rss_mb,
                m.hwm_mb
            );
        }
    }

    let phase_a_s = t_phase_a.elapsed().as_secs_f64();
    println!("Phase A finished in {:.2} s", phase_a_s);

    // Verify atomic session persistence round-trip.
    let session_path = output_dir.join(".photocut.json");
    let session = Session {
        input_dir: input_dir.clone(),
        output_dir: output_dir.clone(),
        export_format: ExportFormat::Png,
        scans: scan_entries.clone(),
    };
    session
        .save_atomic(&session_path)
        .expect("save session atomically");
    let reloaded = Session::load(&session_path).expect("reload session");
    let session_roundtrip_ok = reloaded == session;
    assert!(session_roundtrip_ok, "Session round-trip mismatch!");
    println!(
        "Session round-trip verified: {} ({} scan entries, {} bytes)",
        session_path.display(),
        reloaded.scans.len(),
        fs::metadata(&session_path).map(|m| m.len()).unwrap_or(0)
    );

    // Verify every exported file exists and decodes cleanly back with exact dimensions.
    let mut fmt_counts = [0usize; 3]; // PNG, TIFF, JPEG
    let mut all_exports_verified = true;
    for rec in &exported_records {
        let decoded = image::open(&rec.path).unwrap_or_else(|e| {
            panic!("failed to decode exported {}: {e}", rec.path.display())
        });
        let got_dims = (decoded.width(), decoded.height());
        if got_dims != rec.expected_dims || rec.bytes == 0 {
            eprintln!(
                "Export mismatch on {}: expected {:?}, got {:?}, bytes={}",
                rec.path.display(),
                rec.expected_dims,
                got_dims,
                rec.bytes
            );
            all_exports_verified = false;
        }
        match rec.format_name {
            "PNG" => fmt_counts[0] += 1,
            "TIFF" => fmt_counts[1] += 1,
            _ => fmt_counts[2] += 1,
        }
    }
    println!(
        "Verified {} full-res 600 DPI Bicubic exports (PNG: {}, TIFF: {}, JPEG96: {}) -> all decoded cleanly!",
        exported_records.len(),
        fmt_counts[0],
        fmt_counts[1],
        fmt_counts[2]
    );

    // Phase B: Memory-leak stability loop (15 scans + 150 rapid consecutive slider cycles).
    println!();
    println!("--- Phase B: Memory-Leak Stability Loop (15 scans + 150 rapid preview/adjustment cycles) ---");
    let t_phase_b = Instant::now();
    let mem_b_start = read_mem_stats();
    println!(
        "  [Phase B start] cache={}/{} | VmRSS={:.1} MB | VmHWM={:.1} MB",
        cache.len(),
        cache.capacity(),
        mem_b_start.rss_mb,
        mem_b_start.hwm_mb
    );

    let mut slider_cycles = 0usize;
    let loop_scans = 15.min(files.len());
    for i in 0..loop_scans {
        let path = &files[i];
        let scan = cache.load_or_get(path).expect("reload scan in Phase B");
        max_cache_len = max_cache_len.max(cache.len());
        assert!(cache.len() <= 3);

        let photo = &scan_entries[i].photos[0];
        // 10 rapid slider updates per scan => 150 total rapid consecutive cycles.
        for step in 0..10 {
            let t = step as f32 / 9.0;
            let adj = Adjustments {
                auto_color_strength: t,
                brightness: (t - 0.5) * 0.4,
                contrast: (t - 0.5) * 0.3,
                saturation: (t - 0.5) * 0.25,
                temperature: (t - 0.5) * 0.2,
                tint: (0.5 - t) * 0.15,
            };
            let mut preview = extract_photo(
                &scan,
                photo.crop,
                photo.turn,
                neutral_adjustments(),
                SampleQuality::Bilinear,
                Some(PREVIEW_MAX_DIM),
            );
            apply_adjustments(&mut preview, adj, None);
            assert!(preview.width() > 0 && preview.height() > 0);
            slider_cycles += 1;
        }

        if i + 1 == 8 {
            let m = read_mem_stats();
            println!(
                "  [Phase B mid   8/15, cycles={:>3}] cache={}/{} | VmRSS={:.1} MB | VmHWM={:.1} MB",
                slider_cycles,
                cache.len(),
                cache.capacity(),
                m.rss_mb,
                m.hwm_mb
            );
        }
    }

    let mem_b_end = read_mem_stats();
    println!(
        "  [Phase B end  15/15, cycles={:>3}] cache={}/{} | VmRSS={:.1} MB | VmHWM={:.1} MB ({:.2} s)",
        slider_cycles,
        cache.len(),
        cache.capacity(),
        mem_b_end.rss_mb,
        mem_b_end.hwm_mb,
        t_phase_b.elapsed().as_secs_f64()
    );

    cache.clear();
    let mem_after_clear = read_mem_stats();
    println!(
        "  [After cache.clear()] cache={}/{} ({} bytes) | VmRSS={:.1} MB | VmHWM={:.1} MB",
        cache.len(),
        cache.capacity(),
        cache.estimated_bytes(),
        mem_after_clear.rss_mb,
        mem_after_clear.hwm_mb
    );

    let rss_drift_mb = mem_b_end.rss_mb - rss_after_warmup;

    println!();
    println!("=== Orientation Distribution (150 photos, min_confidence={:.2}) ===", AI_MIN_CONFIDENCE);
    println!(
        "  Deg0: {} | Deg90: {} | Deg180: {} | Deg270: {} | Below threshold (<0.70): {}",
        turn_counts[0], turn_counts[1], turn_counts[2], turn_counts[3], low_conf_count
    );
    if ai_model.is_some() && total_photos > 0 {
        println!(
            "  Avg top confidence: {:.2}% | Total AI inference time: {:.2} s ({:.1} ms/photo)",
            (sum_conf / total_photos as f64) * 100.0,
            ai_infer_wall_ms / 1000.0,
            ai_infer_wall_ms / total_photos as f64
        );
    }

    println!();
    println!("=== Sample Full-Res Export Timings (first 6) ===");
    for rec in exported_records.iter().take(6) {
        println!(
            "  {:<6} {} | {}x{} | {:.2} MB | {:.1} ms",
            rec.format_name,
            rec.path.display(),
            rec.expected_dims.0,
            rec.expected_dims.1,
            rec.bytes as f64 / (1024.0 * 1024.0),
            rec.elapsed_ms
        );
    }

    println!();
    println!("=== Final PASS/FAIL Assertions ===");
    let checks = [
        ("total_scans == 41", files.len() == 41, format!("{}", files.len())),
        ("total_photos == 150", total_photos == 150, format!("{total_photos}")),
        ("anomalies == 0", anomalies.is_empty(), format!("{}", anomalies.len())),
        ("max_cache_len <= 3", max_cache_len <= 3, format!("{max_cache_len}")),
        ("slider_cycles == 150", slider_cycles == 150, format!("{slider_cycles}")),
        ("session_roundtrip_ok", session_roundtrip_ok, format!("{session_roundtrip_ok}")),
        (
            "all_exports_verified (PNG+TIFF+JPEG)",
            all_exports_verified && fmt_counts[0] > 0 && fmt_counts[1] > 0 && fmt_counts[2] > 0,
            format!("{} files ({}/{}/{})", exported_records.len(), fmt_counts[0], fmt_counts[1], fmt_counts[2]),
        ),
        (
            "rss_end_phase_b - rss_after_warmup <= 120 MB",
            rss_drift_mb <= 120.0,
            format!("{:+.1} MB ({:.1} -> {:.1} MB)", rss_drift_mb, rss_after_warmup, mem_b_end.rss_mb),
        ),
    ];

    let mut all_pass = true;
    for (name, ok, detail) in &checks {
        println!("  [{}] {:<46} => {}", if *ok { "PASS" } else { "FAIL" }, name, detail);
        if !*ok {
            all_pass = false;
        }
    }

    println!(
        "\nTotal suite wall time: {:.2} s | Overall: {}",
        t_total.elapsed().as_secs_f64(),
        if all_pass { "PASS" } else { "FAIL" }
    );

    assert!(all_pass, "One or more stress/verification checks failed!");
}
