//! Persistencia no destructiva de sesiones `.photocut.json`.
//!
//! Guarda atómicamente (escribiendo a un archivo `.tmp` en el mismo directorio y
//! renombrando) para evitar corrupción del progreso si ocurre un cierre inesperado.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{DetectedPhoto, ExportFormat};

/// Registro persistido de un escaneo individual y sus fotos detectadas/editadas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanEntry {
    pub rel_path: String,
    pub photos: Vec<DetectedPhoto>,
}

/// Estado completo de una sesión de recorte y restauración.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub export_format: ExportFormat,
    pub scans: Vec<ScanEntry>,
}

impl Session {
    /// Carga una sesión desde un archivo JSON en disco.
    pub fn load(path: &Path) -> io::Result<Self> {
        let data = fs::read(path)?;
        serde_json::from_slice(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Guarda la sesión atómicamente escribiendo primero a `<path>.tmp` y luego renombrando.
    pub fn save_atomic(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let tmp_path = temp_sibling_path(path);
        {
            let mut file = File::create(&tmp_path)?;
            file.write_all(&json)?;
            file.sync_all()?;
        }

        fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

fn temp_sibling_path(path: &Path) -> PathBuf {
    let mut name: OsString = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("session"));
    name.push(".tmp");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Adjustments, OrientedRect, QuarterTurn, ReviewStatus};

    fn sample_session(status: ReviewStatus, quality: u8) -> Session {
        Session {
            input_dir: PathBuf::from("/scans/album_01"),
            output_dir: PathBuf::from("/exports/album_01"),
            export_format: ExportFormat::Jpeg { quality },
            scans: vec![ScanEntry {
                rel_path: "Escaneo/01/img001.jpg".to_string(),
                photos: vec![DetectedPhoto {
                    id: 0,
                    crop: OrientedRect {
                        cx: 1200.5,
                        cy: 1500.25,
                        width: 2100.0,
                        height: 2950.0,
                        angle_deg: -1.25,
                    },
                    turn: QuarterTurn::Deg90,
                    adjustments: Adjustments {
                        auto_color_strength: 0.7,
                        brightness: 0.05,
                        contrast: 0.1,
                        saturation: -0.05,
                        temperature: 0.02,
                        tint: -0.08,
                    },
                    status,
                }],
            }],
        }
    }

    #[test]
    fn round_trip_save_and_load() {
        let dir = std::env::temp_dir().join(format!("photocut_session_rt_{}", std::process::id()));
        let path = dir.join(".photocut.json");

        let original = sample_session(ReviewStatus::Approved, 96);
        original.save_atomic(&path).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded, original);
        assert!(!temp_sibling_path(&path).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_overwrite_replaces_existing_session_cleanly() {
        let dir = std::env::temp_dir().join(format!("photocut_session_ow_{}", std::process::id()));
        let path = dir.join(".photocut.json");

        let v1 = sample_session(ReviewStatus::Pending, 92);
        v1.save_atomic(&path).unwrap();
        assert_eq!(Session::load(&path).unwrap(), v1);

        let mut v2 = sample_session(ReviewStatus::Approved, 98);
        v2.export_format = ExportFormat::Png;
        v2.scans.push(ScanEntry {
            rel_path: "Escaneo/01/img002.jpg".to_string(),
            photos: Vec::new(),
        });
        v2.save_atomic(&path).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded, v2);
        assert!(!temp_sibling_path(&path).exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
