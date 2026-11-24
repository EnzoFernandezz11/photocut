//! Caché LRU acotada para escaneos de alta resolución en memoria.
//!
//! Evita fugas de memoria al navegar carpetas con decenas de escaneos 600 DPI
//! (~100 MB descomprimidos cada uno) manteniendo solo los más recientes en RAM.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::{ImageError, RgbImage};

/// Caché LRU acotada de escaneos RGB decodificados.
pub struct ScanCache {
    max_entries: usize,
    /// Ordenados del menos reciente (`[0]`) al más reciente (`[len - 1]`).
    entries: Vec<(PathBuf, Arc<RgbImage>)>,
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new(3)
    }
}

impl ScanCache {
    /// Crea una caché con capacidad máxima `max_entries`.
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            entries: Vec::with_capacity(max_entries),
        }
    }

    /// Devuelve la imagen desde la caché (marcándola como más reciente) o la carga de disco.
    pub fn load_or_get(&mut self, path: &Path) -> Result<Arc<RgbImage>, ImageError> {
        if let Some(hit) = self.get(path) {
            return Ok(hit);
        }

        let img = Arc::new(image::open(path)?.to_rgb8());
        self.insert(path.to_path_buf(), Arc::clone(&img));
        Ok(img)
    }

    /// Obtiene una entrada existente y actualiza su recencia (touch LRU).
    pub fn get(&mut self, path: &Path) -> Option<Arc<RgbImage>> {
        let pos = self.entries.iter().position(|(p, _)| p == path)?;
        let entry = self.entries.remove(pos);
        let img = Arc::clone(&entry.1);
        self.entries.push(entry);
        Some(img)
    }

    /// Inserta o actualiza una entrada en la caché, expulsando la menos reciente si está llena.
    pub fn insert(&mut self, path: PathBuf, img: Arc<RgbImage>) {
        if self.max_entries == 0 {
            return;
        }
        if let Some(pos) = self.entries.iter().position(|(p, _)| p == &path) {
            self.entries.remove(pos);
        } else if self.entries.len() >= self.max_entries {
            self.entries.remove(0);
        }
        self.entries.push((path, img));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.max_entries
    }

    /// Memoria aproximada (bytes) ocupada por los búferes RGB cacheados.
    pub fn estimated_bytes(&self) -> usize {
        self.entries.iter().map(|(_, img)| img.as_raw().len()).sum()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn dummy_img(val: u8) -> Arc<RgbImage> {
        Arc::new(RgbImage::from_pixel(4, 4, Rgb([val, val, val])))
    }

    #[test]
    fn evicts_least_recently_used_entry() {
        let mut cache = ScanCache::new(3);
        cache.insert(PathBuf::from("a.jpg"), dummy_img(1));
        cache.insert(PathBuf::from("b.jpg"), dummy_img(2));
        cache.insert(PathBuf::from("c.jpg"), dummy_img(3));

        // Tocar "a.jpg" la mueve a más reciente; ahora "b.jpg" es la más antigua.
        let hit_a = cache.get(Path::new("a.jpg")).expect("a.jpg cached");
        assert_eq!(hit_a.get_pixel(0, 0).0[0], 1);

        // Insertar "d.jpg" debe expulsar "b.jpg".
        cache.insert(PathBuf::from("d.jpg"), dummy_img(4));
        assert_eq!(cache.len(), 3);
        assert!(cache.get(Path::new("b.jpg")).is_none());
        assert!(cache.get(Path::new("a.jpg")).is_some());
        assert!(cache.get(Path::new("c.jpg")).is_some());
        assert!(cache.get(Path::new("d.jpg")).is_some());
    }

    #[test]
    fn never_exceeds_capacity_under_many_inserts() {
        let mut cache = ScanCache::default();
        assert_eq!(cache.capacity(), 3);

        for i in 0..25u8 {
            cache.insert(PathBuf::from(format!("img_{i:02}.jpg")), dummy_img(i));
            assert!(cache.len() <= cache.capacity());
        }

        assert_eq!(cache.len(), 3);
        assert_eq!(cache.estimated_bytes(), 3 * 4 * 4 * 3);

        // Solo las 3 últimas deben permanecer.
        assert!(cache.get(Path::new("img_21.jpg")).is_none());
        assert!(cache.get(Path::new("img_22.jpg")).is_some());
        assert!(cache.get(Path::new("img_23.jpg")).is_some());
        assert!(cache.get(Path::new("img_24.jpg")).is_some());

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.estimated_bytes(), 0);
    }

    #[test]
    fn load_or_get_caches_decoded_arc() {
        let dir = std::env::temp_dir().join(format!("photocut_cache_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.png");
        RgbImage::from_pixel(8, 8, Rgb([10, 20, 30]))
            .save(&path)
            .unwrap();

        let mut cache = ScanCache::new(2);
        let first = cache.load_or_get(&path).unwrap();
        let second = cache.load_or_get(&path).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.estimated_bytes(), 8 * 8 * 3);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
