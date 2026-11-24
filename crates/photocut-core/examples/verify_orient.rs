use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use photocut_core::detect::{DetectConfig, detect_photos};
use photocut_core::export::{SampleQuality, extract_photo, save_image};
use photocut_core::orient::OrientationModel;
use photocut_core::{Adjustments, ExportFormat, QuarterTurn};

const TARGET_SCANS: &[&str] = &[
    "Escaneo/01/img001.jpg",
    "Escaneo/01/img004.jpg",
    "Escaneo/02/img007.jpg",
    "Escaneo/03/img040.jpg",
    "Escaneo/03/img041.jpg",
];

fn main() {
    let args: Vec<String> = env::args().collect();
    let input_dir = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("data-photo"));
    let output_dir = PathBuf::from(
        args.get(2)
            .map(String::as_str)
            .unwrap_or("tmp/verify/oriented_samples"),
    );

    let model_path = OrientationModel::find_default().unwrap_or_else(|| {
        eprintln!("Orientation ONNX model not found via OrientationModel::find_default()");
        std::process::exit(1);
    });

    println!("Loading ONNX model: {}", model_path.display());
    let t_load = Instant::now();
    let model = OrientationModel::load(&model_path).unwrap_or_else(|e| {
        eprintln!("Failed to load orientation model: {e}");
        std::process::exit(1);
    });
    println!(
        "Model loaded & optimized in {:.1} ms",
        t_load.elapsed().as_secs_f64() * 1000.0
    );

    fs::create_dir_all(&output_dir).expect("create oriented_samples dir");

    let mut total_photos = 0usize;
    let mut total_infer_ms = 0.0f64;

    println!();
    println!(
        "{:<24} | {:>5} | {:<8} | {:>10} | {:>8}",
        "Scan", "Photo", "PredTurn", "Confidence", "Infer ms"
    );
    println!("{}", "-".repeat(67));

    for rel in TARGET_SCANS {
        let scan_path = input_dir.join(rel);
        let img = image::open(&scan_path)
            .unwrap_or_else(|e| panic!("failed to open {}: {e}", scan_path.display()))
            .to_rgb8();
        let photos = detect_photos(&img, DetectConfig::default());
        let stem = scan_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("scan");

        for photo in photos {
            // Fast 512px preview crop without color stretch for orientation classification.
            let crop_for_ai = extract_photo(
                &img,
                photo.crop,
                QuarterTurn::Deg0,
                Adjustments {
                    auto_color_strength: 0.0,
                    ..Default::default()
                },
                SampleQuality::Bilinear,
                Some(512),
            );

            let t_inf = Instant::now();
            let pred = model
                .predict(&crop_for_ai, 0.5)
                .unwrap_or_else(|e| panic!("predict failed on {rel} photo#{}: {e}", photo.id));
            let inf_ms = t_inf.elapsed().as_secs_f64() * 1000.0;

            total_photos += 1;
            total_infer_ms += inf_ms;

            println!(
                "{:<24} | {:>5} | {:<8} | {:>9.2}% | {:>8.1}",
                rel,
                format!("#{}", photo.id),
                format!("{:?}", pred.turn),
                pred.confidence * 100.0,
                inf_ms
            );

            // Save auto-oriented + auto-color enhanced preview (max 800px) for Visual QA.
            let oriented = extract_photo(
                &img,
                photo.crop,
                pred.turn,
                Adjustments::default(),
                SampleQuality::Bicubic,
                Some(800),
            );
            let out_path = output_dir.join(format!(
                "{stem}_photo{}_{:?}.jpg",
                photo.id, pred.turn
            ));
            save_image(&oriented, &out_path, ExportFormat::Jpeg { quality: 92 })
                .expect("save oriented sample");
        }
    }

    println!("{}", "-".repeat(67));
    println!(
        "Total photos: {} | Avg inference: {:.1} ms/photo | Output: {}",
        total_photos,
        total_infer_ms / total_photos.max(1) as f64,
        output_dir.display()
    );
}
