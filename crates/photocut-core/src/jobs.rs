//! Registro persistente de trabajos ("Trabajos") para procesar múltiples carpetas
//! separadas en cola desatendida (ej. dejar corriendo toda la noche) y revisarlas
//! por álbum cuando el usuario vuelva.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::Session;
use crate::{ExportFormat, ReviewStatus};

/// Estado del ciclo de vida de un trabajo.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum JobStatus {
    #[default]
    Pending,
    Processing,
    ReadyForReview,
    Completed,
    Error(String),
}

/// Un trabajo independiente: una carpeta de escaneos con su propia carpeta de salida y sesión.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: u64,
    pub name: String,
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub export_format: ExportFormat,
    pub use_ai_orient: bool,
    pub total_scans: usize,
    pub indexed_scans: usize,
    pub total_photos: usize,
    pub approved_photos: usize,
    pub skipped_photos: usize,
    pub status: JobStatus,
}

impl JobRecord {
    /// Sincroniza contadores y estado leyendo los escaneos en disco y `<output_dir>/.photocut.json`.
    pub fn refresh_from_disk(&mut self) {
        let total_scans = count_scan_images(&self.input_dir, Some(&self.output_dir));
        self.total_scans = total_scans;

        let session_path = self.output_dir.join(".photocut.json");
        if let Ok(session) = Session::load(&session_path) {
            self.export_format = session.export_format;
            self.indexed_scans = session.scans.len().min(total_scans.max(session.scans.len()));
            let mut total_photos = 0usize;
            let mut approved = 0usize;
            let mut skipped = 0usize;
            for scan in &session.scans {
                total_photos += scan.photos.len();
                for p in &scan.photos {
                    match p.status {
                        ReviewStatus::Approved => approved += 1,
                        ReviewStatus::Skipped => skipped += 1,
                        ReviewStatus::Pending => {}
                    }
                }
            }
            self.total_photos = total_photos;
            self.approved_photos = approved;
            self.skipped_photos = skipped;

            if !matches!(self.status, JobStatus::Processing) {
                self.status = compute_idle_status(
                    self.total_scans,
                    self.indexed_scans,
                    self.total_photos,
                    self.approved_photos,
                    self.skipped_photos,
                );
            }
        } else if !matches!(self.status, JobStatus::Processing | JobStatus::Error(_)) {
            self.indexed_scans = 0;
            self.total_photos = 0;
            self.approved_photos = 0;
            self.skipped_photos = 0;
            self.status = JobStatus::Pending;
        }
    }

    pub fn pending_photos(&self) -> usize {
        self.total_photos
            .saturating_sub(self.approved_photos + self.skipped_photos)
    }

    pub fn needs_indexing(&self) -> bool {
        self.total_scans == 0 || self.indexed_scans < self.total_scans
    }
}

/// Calcula el estado en reposo a partir de los contadores de indexado y revisión.
pub fn compute_idle_status(
    total_scans: usize,
    indexed_scans: usize,
    total_photos: usize,
    approved_photos: usize,
    skipped_photos: usize,
) -> JobStatus {
    if indexed_scans == 0 {
        JobStatus::Pending
    } else if total_scans > 0 && indexed_scans < total_scans {
        JobStatus::Pending
    } else if total_photos > 0 && (approved_photos + skipped_photos) >= total_photos {
        JobStatus::Completed
    } else {
        JobStatus::ReadyForReview
    }
}

/// Lista persistente de todos los trabajos del usuario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRegistry {
    pub next_id: u64,
    pub jobs: Vec<JobRecord>,
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self {
            next_id: 1,
            jobs: Vec::new(),
        }
    }
}

impl JobRegistry {
    /// Ruta por defecto del registro de trabajos (multiplataforma Linux + Windows).
    pub fn default_path() -> PathBuf {
        if let Ok(custom) = std::env::var("PHOTOCUT_JOBS_FILE")
            && !custom.trim().is_empty()
        {
            return PathBuf::from(custom);
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA")
                && !appdata.trim().is_empty()
            {
                return PathBuf::from(appdata).join("PhotoCut").join("jobs.json");
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
                && !xdg.trim().is_empty()
            {
                return PathBuf::from(xdg).join("photocut").join("jobs.json");
            }
            if let Ok(home) = std::env::var("HOME")
                && !home.trim().is_empty()
            {
                return PathBuf::from(home)
                    .join(".config")
                    .join("photocut")
                    .join("jobs.json");
            }
        }
        PathBuf::from(".photocut_jobs.json")
    }

    pub fn load_or_default(path: &Path) -> Self {
        let Ok(data) = fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&data).unwrap_or_default()
    }

    /// Guarda atómicamente (`<path>.tmp` + `sync_all` + `rename`) para evitar corrupción.
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

    /// Agrega un trabajo nuevo o devuelve el `id` existente si ya existe para el mismo `input_dir`.
    pub fn add_or_update_job(
        &mut self,
        name: String,
        input_dir: PathBuf,
        output_dir: PathBuf,
        export_format: ExportFormat,
        use_ai_orient: bool,
    ) -> u64 {
        if let Some(existing) = self
            .jobs
            .iter_mut()
            .find(|j| j.input_dir == input_dir && j.output_dir == output_dir)
        {
            if !name.trim().is_empty() {
                existing.name = name;
            }
            existing.export_format = export_format;
            existing.use_ai_orient = use_ai_orient;
            existing.refresh_from_disk();
            return existing.id;
        }

        let id = self.next_id;
        self.next_id += 1;

        let display_name = if name.trim().is_empty() {
            suggest_job_name(&input_dir)
        } else {
            name.trim().to_string()
        };

        let mut job = JobRecord {
            id,
            name: display_name,
            input_dir,
            output_dir,
            export_format,
            use_ai_orient,
            total_scans: 0,
            indexed_scans: 0,
            total_photos: 0,
            approved_photos: 0,
            skipped_photos: 0,
            status: JobStatus::Pending,
        };
        job.refresh_from_disk();
        self.jobs.push(job);
        id
    }

    pub fn remove_job(&mut self, id: u64) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|j| j.id != id);
        self.jobs.len() != before
    }

    pub fn find(&self, id: u64) -> Option<&JobRecord> {
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut JobRecord> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    pub fn refresh_all(&mut self) {
        for job in &mut self.jobs {
            job.refresh_from_disk();
        }
    }
}

/// Deriva una carpeta de salida por defecto junto a la de entrada (`<carpeta>_recortadas`).
pub fn default_output_dir(input_dir: &Path) -> PathBuf {
    let mut name = input_dir
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("fotos"));
    name.push("_recortadas");
    match input_dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Sugiere un nombre legible para un trabajo a partir de su carpeta.
pub fn suggest_job_name(input_dir: &Path) -> String {
    input_dir
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Trabajo")
        .to_string()
}

/// Descubre subcarpetas inmediatas que contengan al menos una imagen soportada,
/// para importar múltiples álbumes como trabajos separados en un solo paso.
pub fn discover_job_subfolders(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            if !p.is_dir() {
                return false;
            }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            if name.starts_with('.') || name.ends_with("_recortadas") {
                return false;
            }
            count_scan_images(p, None) > 0
        })
        .collect();
    folders.sort();
    folders
}

/// Cuenta recursivamente las imágenes de escaneo soportadas dentro de `dir`.
pub fn count_scan_images(dir: &Path, exclude_dir: Option<&Path>) -> usize {
    let canonical_exclude = exclude_dir.and_then(|p| fs::canonicalize(p).ok());
    let mut count = 0usize;
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        if let Some(ref ex) = canonical_exclude
            && let Ok(curr_can) = fs::canonicalize(&current)
            && curr_can == *ex
        {
            continue;
        }
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.starts_with('.')
            {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if is_supported_scan_ext(&path) {
                count += 1;
            }
        }
    }
    count
}

fn is_supported_scan_ext(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff"
    )
}

fn temp_sibling_path(path: &Path) -> PathBuf {
    let mut name: OsString = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("jobs"));
    name.push(".tmp");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trip_and_dedup() {
        let base = std::env::temp_dir().join(format!("photocut_jobs_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let in1 = base.join("01");
        let in2 = base.join("02");
        fs::create_dir_all(&in1).unwrap();
        fs::create_dir_all(&in2).unwrap();
        fs::write(in1.join("a.jpg"), b"fake").unwrap();
        fs::write(in2.join("b.png"), b"fake").unwrap();

        let subs = discover_job_subfolders(&base);
        assert_eq!(subs, vec![in1.clone(), in2.clone()]);

        let reg_path = base.join("jobs.json");
        let mut reg = JobRegistry::default();
        let id1 = reg.add_or_update_job(
            "Álbum 01".into(),
            in1.clone(),
            default_output_dir(&in1),
            ExportFormat::Png,
            true,
        );
        let id1_dup = reg.add_or_update_job(
            "Álbum 01 Renombrado".into(),
            in1.clone(),
            default_output_dir(&in1),
            ExportFormat::Tiff,
            false,
        );
        assert_eq!(id1, id1_dup);
        assert_eq!(reg.jobs.len(), 1);
        assert_eq!(reg.jobs[0].total_scans, 1);
        assert_eq!(reg.jobs[0].export_format, ExportFormat::Tiff);

        reg.save_atomic(&reg_path).unwrap();
        let loaded = JobRegistry::load_or_default(&reg_path);
        assert_eq!(loaded, reg);

        assert!(reg.remove_job(id1));
        assert!(reg.jobs.is_empty());

        let _ = fs::remove_dir_all(&base);
    }
}
