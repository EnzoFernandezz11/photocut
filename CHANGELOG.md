# Historial de versiones (Changelog)

La versión única de toda la app se define en `[workspace.package].version` dentro de `Cargo.toml` y se muestra automáticamente en la ventana y el menú principal.

## [v0.1.0] - 2026-11-24
Primera versión funcional (v0) multiplataforma (**Linux y Windows**):

- **Motor puro en Rust (`photocut-core`):**
  - Detección automática multi-foto con perfil local 1D de fondo del escáner y enderezado (*deskew*) por rectángulo orientado mínimo.
  - Restauración automática de color y luminosidad para fotos antiguas + 6 ajustes manuales no destructivos.
  - Auto-orientación local **0°/90°/180°/270°** con ONNX (`tract-onnx`, opcional y 100% offline).
  - Exportación *Catmull-Rom bicubic* en una sola pasada a máxima resolución (600 DPI) en **PNG sin pérdida**, **TIFF sin pérdida** o **JPEG calidad 80–100**.
  - Caché LRU acotada a 3 escaneos en RAM (`ScanCache`) y guardado atómico de sesión (`.photocut.json`).
- **App de escritorio (`photocut-app` en `egui`):**
  - Menú principal **"Trabajos"** con creación por carpeta, importación de subcarpetas en 1 clic y cola desatendida para dejar procesando horas/toda la noche.
  - Revisión interactiva con **Re-recortar (`C`)** sobre el escaneo original (4 esquinas, 4 bordes, mover y rotar), **Antes/Después (`Espacio`)**, iconos vectoriales en todos los botones y tooltips claros en español.
  - Icono propio de aplicación embebido para ventana/barra de tareas (Linux + Windows) y recurso `.ico` en Windows.
- **Calidad y CI:**
  - 34 tests unitarios y de estrés (anti-cuelgues y memoria acotada) + workflow de GitHub Actions para Linux y Windows.
