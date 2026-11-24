//! Detección local de orientación gruesa (0°/90°/180°/270°) con modelo ONNX en Rust puro.
//!
//! Utiliza `tract-onnx` sin dependencias C++/DLL externas. El modelo es opcional:
//! si el archivo `.onnx` no está presente, la aplicación sigue funcionando con rotación manual.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::RgbImage;
use image::imageops::FilterType;
use tract_onnx::prelude::*;

use crate::QuarterTurn;

const MODEL_FILENAME: &str = "orientation_model_v2_0.9882.onnx";
const RESIZE_DIM: u32 = 416;
const INPUT_DIM: u32 = 384;
const CROP_OFFSET: u32 = (RESIZE_DIM - INPUT_DIM) / 2;
const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Resultado de predicción de orientación para una foto recortada.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrientationPrediction {
    /// Rotación horaria sugerida para enderezar la foto (`Deg0` si `confidence < min_confidence`).
    pub turn: QuarterTurn,
    /// Probabilidad softmax de la clase ganadora (0.0..=1.0).
    pub confidence: f32,
}

/// Modelo EfficientNetV2-S cargado en memoria, seguro para compartir en `Arc` entre hilos.
pub struct OrientationModel {
    plan: Arc<TypedRunnableModel>,
}

impl OrientationModel {
    /// Carga y optimiza el modelo ONNX desde `path` para inferencia fija `[1, 3, 384, 384]`.
    pub fn load(path: &Path) -> Result<Self, String> {
        let model = tract_onnx::onnx()
            .model_for_path(path)
            .map_err(|e| format!("failed to load ONNX model {}: {e}", path.display()))?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(
                    f32::datum_type(),
                    tvec![1usize, 3, INPUT_DIM as usize, INPUT_DIM as usize],
                ),
            )
            .map_err(|e| format!("failed to set input shape: {e}"))?
            .into_optimized()
            .map_err(|e| format!("failed to optimize ONNX graph: {e}"))?
            .into_runnable()
            .map_err(|e| format!("failed to build runnable plan: {e}"))?;

        Ok(Self { plan: model })
    }

    /// Busca automáticamente el modelo en `PHOTOCUT_ORIENT_MODEL`, `./models/...`
    /// o junto al ejecutable en `<exe_dir>/models/...`.
    pub fn find_default() -> Option<PathBuf> {
        if let Ok(env_path) = env::var("PHOTOCUT_ORIENT_MODEL") {
            let p = PathBuf::from(env_path);
            if p.is_file() {
                return Some(p);
            }
        }

        let cwd_candidate = PathBuf::from("models").join(MODEL_FILENAME);
        if cwd_candidate.is_file() {
            return Some(cwd_candidate);
        }

        if let Ok(exe) = env::current_exe()
            && let Some(exe_dir) = exe.parent()
        {
            let sibling = exe_dir.join("models").join(MODEL_FILENAME);
            if sibling.is_file() {
                return Some(sibling);
            }
        }

        None
    }

    /// Predice la rotación necesaria para poner derecha `cropped_rgb`.
    /// Si la confianza máxima es menor que `min_confidence`, devuelve `QuarterTurn::Deg0`.
    pub fn predict(
        &self,
        cropped_rgb: &RgbImage,
        min_confidence: f32,
    ) -> Result<OrientationPrediction, String> {
        let data = preprocess_nchw(cropped_rgb);
        let input = Tensor::from_shape(
            &[1, 3, INPUT_DIM as usize, INPUT_DIM as usize],
            &data,
        )
        .map_err(|e| format!("failed to build input tensor: {e}"))?;

        let outputs = self
            .plan
            .run(tvec!(input.into()))
            .map_err(|e| format!("ONNX inference failed: {e}"))?;

        let logits = outputs
            .first()
            .ok_or_else(|| "ONNX model returned no outputs".to_string())?
            .to_plain_array_view::<f32>()
            .map_err(|e| format!("failed to read f32 logits: {e}"))?;

        let slice = logits
            .as_slice()
            .ok_or_else(|| "non-contiguous output logits".to_string())?;
        if slice.len() < 4 {
            return Err(format!("expected 4 logits, got {}", slice.len()));
        }

        let probs = softmax4([slice[0], slice[1], slice[2], slice[3]]);
        let (best_idx, confidence) = probs
            .iter()
            .copied()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or((0, 0.0));

        let turn = if confidence < min_confidence {
            QuarterTurn::Deg0
        } else {
            class_to_turn(best_idx)
        };

        Ok(OrientationPrediction { turn, confidence })
    }
}

/// Redimensiona a 416×416, recorta el centro 384×384 y normaliza RGB en orden NCHW.
pub(crate) fn preprocess_nchw(img: &RgbImage) -> Vec<f32> {
    let resized = image::imageops::resize(img, RESIZE_DIM, RESIZE_DIM, FilterType::Triangle);
    let dim = INPUT_DIM as usize;
    let plane = dim * dim;
    let mut out = vec![0.0f32; 3 * plane];

    let raw = resized.as_raw();
    let stride = RESIZE_DIM as usize * 3;
    let off = CROP_OFFSET as usize;

    for y in 0..dim {
        let src_row = (y + off) * stride;
        let dst_row = y * dim;
        for x in 0..dim {
            let si = src_row + (x + off) * 3;
            let di = dst_row + x;
            out[di] = (raw[si] as f32 / 255.0 - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
            out[plane + di] = (raw[si + 1] as f32 / 255.0 - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
            out[2 * plane + di] =
                (raw[si + 2] as f32 / 255.0 - IMAGENET_MEAN[2]) / IMAGENET_STD[2];
        }
    }

    out
}

pub(crate) fn softmax4(logits: [f32; 4]) -> [f32; 4] {
    let max_v = logits
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let exps = logits.map(|v| (v - max_v).exp());
    let sum: f32 = exps.iter().sum();
    if sum <= 0.0 || !sum.is_finite() {
        return [0.25; 4];
    }
    exps.map(|e| e / sum)
}

fn class_to_turn(class_idx: usize) -> QuarterTurn {
    match class_idx {
        0 => QuarterTurn::Deg0,
        1 => QuarterTurn::Deg90,
        2 => QuarterTurn::Deg180,
        3 => QuarterTurn::Deg270,
        _ => QuarterTurn::Deg0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn orientation_model_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OrientationModel>();
    }

    #[test]
    fn preprocess_nchw_has_exact_shape_and_imagenet_normalization() {
        // Imagen uniforme blanca -> (1.0 - mean) / std por canal.
        let white = RgbImage::from_pixel(64, 48, Rgb([255, 255, 255]));
        let buf = preprocess_nchw(&white);
        let plane = (INPUT_DIM * INPUT_DIM) as usize;
        assert_eq!(buf.len(), 3 * plane);

        let exp_r = (1.0 - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
        let exp_g = (1.0 - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
        let exp_b = (1.0 - IMAGENET_MEAN[2]) / IMAGENET_STD[2];
        assert!((buf[0] - exp_r).abs() < 1e-4);
        assert!((buf[plane] - exp_g).abs() < 1e-4);
        assert!((buf[2 * plane] - exp_b).abs() < 1e-4);
    }

    #[test]
    fn softmax4_sums_to_one_and_is_shift_invariant() {
        let p1 = softmax4([1.0, 3.0, 2.0, 0.0]);
        let p2 = softmax4([1001.0, 1003.0, 1002.0, 1000.0]);
        let sum: f32 = p1.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        assert!(p1[1] > p1[2] && p1[2] > p1[0] && p1[0] > p1[3]);
        for i in 0..4 {
            assert!((p1[i] - p2[i]).abs() < 1e-6);
        }
    }
}
