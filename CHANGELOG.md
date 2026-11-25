# Historial de versiones (Changelog)

La versión única de toda la app se define en `[workspace.package].version` dentro de `Cargo.toml` y se muestra automáticamente en la ventana y el menú principal.

## [v0.1.3] - 2026-11-25
Arreglo de arranque en Windows:

- En Windows la ventana ahora se abre con **DirectX 12** (`wgpu`), que funciona incluso en PCs sin drivers de video o máquinas virtuales. Si falla, la app se reinicia sola con OpenGL.
- Si no puede abrir de ninguna forma, muestra un cartel con el error y guarda `photocut-error.log` al lado del `.exe` (antes se cerraba sin mostrar nada).

## [v0.1.2] - 2026-11-25
Re-recorte interactivo renovado y mucho más cómodo sobre la hoja completa:

- **Agarre instantáneo de esquinas y bordes completos:** corregido el bug de umbral de arrastre usando `press_origin` exacto, radio de agarre ampliado a **22 px** en esquinas y detección por distancia punto-segmento en los **4 bordes completos** (ya no solo en el punto medio).
- **Claridad visual inmediata:** marco verde brillante (`#22C55E`, 3 px), 4 círculos grandes en las esquinas con anillo blanco + borde oscuro, máscara gris translúcida más nítida fuera del recorte y contornos secundarios más sutiles.
- **Zoom y paneo suaves en Re-recortar:** rueda del mouse centrada en el cursor (`1x–6x`), paneo arrastrando fuera del recorte (o con botón medio/derecho), botones **`Hoja completa [F]`** y **`Enfocar foto`** en la barra superior, y eliminación de solicitudes repetidas de fondo mientras carga el escaneo.

## [v0.1.1] - 2026-11-24
Mejora mayor del algoritmo de recorte automático (Etapa 2 de refinamiento fino):

- **Refinamiento fino de ángulo (Theil-Sen)** sobre el recorte rotado, inmune a esquinas redondeadas o rotas.
- **Barrido independiente en los 4 bordes** que elimina franjas blancas causadas por sombras de papel curvado sobre el escáner y márgenes blancos impresos, sin morder zonas blancas reales de la foto (cielos, arcos, ropa blanca).
- **Validación en los 41 escaneos (150 fotos / 600 bordes):** bajó de **26/150 fotos** con borde blanco (≥70%) a **0/150**, manteniendo **0 anomalías** y 36/36 tests pasando.

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
