# Plan de Arquitectura y Desarrollo — PhotoCut

## 1. Análisis de las fotos de muestra (`data-photo/`)

Se analizaron los **41 escaneos reales** en `data-photo/Escaneo/{01,02,03}`:

- **Resolución y formato:** 100% JPEG a **600 DPI (`5100 × 7019 px`, ~35.8 MP por escaneo)**, sin tag EXIF de orientación (~363 MB en total).
- **Fondo del escáner:** tapa blanca uniforme (~RGB 250–255).
- **Cantidad por escaneo:** variable (1, 2, 4 y hasta 6 fotos en una misma hoja).
- **Casos borde reales detectados (críticos para el algoritmo):**
  1. **Inclinación leve (0.5°–3°):** casi todas las fotos están ligeramente torcidas → requiere *deskew* (rectángulo orientado de área mínima).
  2. **Orientación 90° / 180° / 270° mezclada:** en una misma hoja hay fotos acostadas y boca abajo (`img001.jpg`, `img007.jpg`, `img040.jpg`, `img041.jpg`).
  3. **Bordes claros sobre fondo blanco:** cielos claros, paredes o ropa blanca tocando el borde de la foto (`img040.jpg`, `img041.jpg`, `img001.jpg`). Un umbral simple de brillo "come" parte de la foto.
  4. **Fotos tocando el borde del escáner:** en `img003.jpg` e `img040.jpg` una foto llega hasta el margen inferior de la imagen → el detector debe agregar un padding blanco sintético antes de buscar contornos.
  5. **Dominantes de color y desgaste vintage:** varias fotos antiguas tienen fuerte viraje al magenta/rojo y bajo contraste (`img001.jpg`, `img004.jpg`, `img007.jpg`).

---

## 2. Decisión de Stack Técnico

### Motor + UI: **100% Rust (`photocut-core` + `egui/eframe`)**

| Componente | Elección recomendada | Por qué |
| :--- | :--- | :--- |
| **Lenguaje** | **Rust** (workspace de 2 crates) | Rendimiento nativo comparable a C/C++, paralelismo seguro con `rayon` (procesar 36 MP rápido en todos los núcleos) y compilación simple en **Linux y Windows**. |
| **Visión / Imagen** | **Rust puro** (`zune-jpeg`, `image`, `fast_image_resize`, `imageproc`, `rayon`) | Evita el dolor de compilar y distribuir DLLs/headers de OpenCV C++ en Windows + Linux. Para detección de rectángulos, warps de alta calidad y niveles/color, Rust puro con SIMD es igual de rápido y da un binario portable sin dependencias externas. |
| **IA Local (opcional/Fase 3)** | **ONNX Runtime (`ort`)** en Rust | **No hace falta IA para recortar** (visión clásica es más precisa y determinista). **Sí sirve IA liviana local** (~10–15 MB en CPU, ~10 ms/foto) para **detectar si la foto está boca abajo o rotada 90°**, algo que OpenCV clásico no puede resolver solo. |
| **Interfaz Desktop** | **`eframe` / `egui`** (nativo GPU) | UI simple, rapidísima (60 FPS al arrastrar esquinas del recorte o mover sliders de color), un solo ejecutable para Windows y Linux sin depender de WebView2/WebKitGTK ni Node. *(Alternativa: Tauri 2 si preferís diseñar la UI con HTML/CSS/React; el núcleo `photocut-core` no cambia nada).* |

---

## 3. Principio Clave: Flujo No Destructivo y Máxima Calidad

Para garantizar **altísima calidad** y que el humano siempre pueda **re-recortar ("volver un poco atrás si cortó de más")**:

1. **Nunca se modifica ni se recorta el escaneo original en disco.**
2. Cada foto detectada se guarda en memoria/sesión como metadatos en **coordenadas del escaneo original (`5100×7019`)**:
   - `OrientedRect` (`cx, cy, width, height, angle_deg`)
   - `QuarterTurn` (`0° / 90° / 180° / 270°`)
   - `Adjustments` (`auto_color_strength`, `brightness`, `contrast`, `saturation`, `temperature`, `tint`)
3. **Vista previa rápida vs. Exportación lossless:**
   - La UI trabaja sobre un preview reducido (~1400–1600 px) → cada slider o rotación responde al instante (<15 ms).
   - Al entrar en modo **"Re-recortar"**, se muestra el escaneo original (con zoom en la zona de esa foto y margen generoso alrededor) con las 4 esquinas/bordes arrastrables + ángulo libre. Nada se perdió.
   - Al presionar **OK / Guardar**, el motor aplica **una sola transformación afín de alta calidad (Catmull-Rom / Lanczos)** + ajustes de color directamente sobre el escaneo original de 600 DPI y escribe el archivo final (**PNG/TIFF sin pérdida** o **JPEG Q98 4:4:4**, conservando 600 DPI).

---

## 4. Plan de Desarrollo por Fases

### Fase 1 — Motor de Detección y Procesamiento (`photocut-core`)
*Objetivo: validar precisión y velocidad sobre los 41 escaneos reales de `data-photo/` antes de escribir la UI.*

1. **Carga rápida y caché:** decodificación rápida con `zune-jpeg` + downscale SIMD (`fast_image_resize`) para detección (~1200 px de alto).
2. **Detector multi-foto robusto (anti-borde-blanco):**
   - Padding blanco sintético de 12 px para fotos pegadas al borde del escáner.
   - Máscara combinada: distancia al blanco del escáner + saturación + magnitud de gradiente (bordes) + cierre morfológico para no morder zonas blancas internas.
   - Componentes conectados + filtrado por área mínima + envolvente convexa y rectángulo orientado de área mínima (*rotating calipers*).
   - Refinamiento fino de bordes para evitar líneas blancas sobrantes o recortes excesivos.
3. **Auto-ajuste de color y luminosidad:**
   - Auto-niveles por canal (percentiles 0.5%–99.5% en R, G, B) mezclable con `auto_color_strength` (corrige el viraje magenta de las muestras sin destruir tonos naturales).
   - Pipeline de ajustes manuales: brillo/exposición, contraste, saturación, temperatura y tinte.
4. **Exportación full-res:** recorte + enderezado en un solo paso sobre la imagen original de 600 DPI + guardado en PNG / TIFF / JPEG Q98.
5. **Benchmark local:** comando/ejemplo en `photocut-core` que procesa los 41 escaneos de `data-photo/` y genera una carpeta temporal de diagnóstico con los rectángulos dibujados para verificar visualmente cuántos recortes salen perfectos de entrada.

### Fase 2 — Interfaz de Escritorio Simple (`photocut-app` con `egui`)
*Objetivo: app funcional en Linux y Windows para revisar lotes reales de punta a punta.*

1. **Selección de carpetas:** elegir carpeta de entrada (escaneos) y carpeta de salida (fotos aprobadas).
2. **Pre-procesamiento en segundo plano:** escanea la carpeta en hilos separados (`rayon`) mientras el usuario ya empieza a revisar la primera foto.
3. **Pantalla principal de revisión (flujo rápido con teclado + mouse):**
   - Vista central grande de la foto recortada y ajustada (con toggle rápido **Antes / Después** manteniendo `Espacio`).
   - Panel lateral simple con controles:
     - Rotar 90° izq/der (`Q` / `E`) y ángulo fino.
     - Botón **Re-recortar (`C`)**: abre el overlay sobre el escaneo original para estirar/mover bordes, esquinas o agregar un recorte manual que el detector haya pasado por alto.
     - Sliders simples: Auto-Color (%), Brillo, Contraste, Saturación, Temperatura, Tinte + botón **Reset**.
     - Formato de salida (PNG / TIFF / JPEG Máx.).
   - Botones de acción: **OK y Guardar (`Enter`)**, **Omitir (`Del`)**, **Anterior/Siguiente (`←`/`→`)**.
4. **Persistencia de sesión:** archivo `.photocut.json` en la carpeta de salida para cerrar la app y retomar otro día sin perder progreso ni re-procesar fotos ya guardadas.

### Fase 3 — Orientación Automática con IA Local (Opcional)
*Objetivo: reducir casi a cero las pulsaciones manuales de `Q`/`E`.*

1. Integrar `ort` (ONNX Runtime) con un modelo liviano de orientación de imágenes (4 clases: 0°/90°/180°/270°, <15 MB, 100% offline en CPU).
2. Si la confianza supera el umbral (ej. >85%), aplica el `QuarterTurn` automáticamente antes de mostrarle la foto al humano; el humano siempre puede corregirlo en un clic.

### Fase 4 — Empaquetado Cross-Platform (Windows + Linux)
1. Compilación optimizada (`cargo build --release` con LTO).
2. Workflow de GitHub Actions que genera ejecutables listos para usar:
   - Windows: `photocut.exe` (x86_64)
   - Linux: binario `photocut` (x86_64)
3. Pruebas con lotes grandes (>100 escaneos) para validar uso de RAM (evitar mantener 100 escaneos de 36 MP descomprimidos a la vez; caché LRU de 2–3 escaneos en memoria).

---

## 5. Decisiones confirmadas

1. **Interfaz de escritorio:** **`egui` / `eframe` (100% Rust)** con estética cuidada estilo cuarto oscuro fotográfico (fondo neutro oscuro para evaluar color sin sesgo, controles limpios, overlay interactivo de re-recorte a 60 FPS).
2. **Formato de exportación:** **configurable desde la interfaz** (PNG sin pérdida, TIFF sin pérdida, o JPEG de alta calidad con selector de calidad 80–100), guardado en las preferencias de sesión.

