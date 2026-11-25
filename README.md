# PhotoCut

Aplicación de escritorio multiplataforma (**Windows y Linux**) escrita **100% en Rust** para separar, enderezar, restaurar color y exportar en máxima calidad (600 DPI) fotos individuales escaneadas sobre fondo blanco, con revisión humana rápida y re-recorte interactivo no destructivo.

## Características principales

- **Detección automática multi-foto y enderezado (*deskew*):** detecta 1, 2, 4, 6 o más fotos por escaneo, compensa sombras de bordes de la tapa del escáner con perfiles locales de fondo 1D y calcula el rectángulo orientado de área mínima.
- **Orientación automática con IA local (opcional, 100% offline):** inferencia en Rust puro (`tract-onnx`, sin DLLs externas) con un modelo EfficientNetV2-S de 4 clases (0°/90°/180°/270°). Si no está presente, la app funciona igual con rotación manual (`Q`/`E`).
- **Restauración automática de color vintage:** estiramiento de niveles por canal (0.3%–99.7% ignorando bordes) + balance suave de gamma de medios tonos para corregir virajes magenta, más controles manuales (brillo, contraste, saturación, temperatura y tinte).
- **Flujo 100% no destructivo y máxima calidad:** nunca modifica el escaneo original; permite entrar en modo **Re-recortar (`C`)** sobre el escaneo completo en cualquier momento y exporta en una sola pasada bicúbica (*Catmull-Rom*) en **PNG**, **TIFF** sin pérdida o **JPEG** alta calidad (80–100).
- **Gestor de Trabajos y cola desatendida multi-álbum:** al iniciar sin argumentos abre el menú **Trabajos**, permitiendo cargar carpetas individuales o importar subcarpetas de una sola vez (cada una con su propia salida `<carpeta>_recortadas`) y dejar la cola procesando toda la noche con guardado atómico tras cada escaneo y limpieza de caché entre carpetas.
- **Memoria acotada y sesiones recuperables:** caché LRU de 3 escaneos en RAM (`ScanCache`) para procesar lotes grandes de 36 MP sin fugas de memoria y guardado atómico en `.photocut.json` para retomar el trabajo sin perder progreso.

## Cómo ejecutar

```bash
# Abrir el Menú Principal (Gestor de Trabajos y cola por lotes)
cargo run --release -p photocut-app

# O abrir directamente una carpeta de escaneos (salida por defecto: <entrada>_recortadas)
cargo run --release -p photocut-app -- --input data-photo

# Especificando también carpeta de salida personalizada
cargo run --release -p photocut-app -- --input data-photo --output ./mis_fotos_exportadas
```

## Atajos de teclado

| Tecla | Acción |
| :--- | :--- |
| `Q` / `E` | Rotar 90° en sentido antihorario / horario |
| `C` | Activar o salir del modo **Re-recortar** sobre el escaneo original |
| `Espacio` *(mantener)* | Comparar **Antes / Después** (muestra el recorte original sin ajustes) |
| `Enter` | **Aprobar y exportar** la foto actual en máxima resolución y avanzar a la siguiente |
| `Del` *(Supr)* | **Omitir** la foto actual y avanzar |
| `←` / `→` | Ir a la foto anterior / siguiente |
| `F` | Restablecer zoom y encuadre de la vista |

## Modelo opcional de orientación automática (IA local)

PhotoCut busca automáticamente el modelo ONNX (EfficientNetV2-S, 4 clases, licencia MIT) en este orden:

1. Variable de entorno `PHOTOCUT_ORIENT_MODEL` (ruta al archivo `.onnx`).
2. `./models/orientation_model_v2_0.9882.onnx` (relativo al directorio actual).
3. `<carpeta_del_ejecutable>/models/orientation_model_v2_0.9882.onnx`.

Para habilitarlo, creá la carpeta `models/` y descargá `orientation_model_v2_0.9882.onnx` allí (la carpeta `models/*.onnx` está excluida en `.gitignore`). Si el archivo no existe, PhotoCut arranca normalmente y permite rotar manualmente con `Q`/`E`.

## Tests y verificación sobre el dataset completo

```bash
# Ejecutar todos los tests unitarios y de estrés (anti-deadlock, coalescing, LRU, sesiones, IA)
cargo test --all-targets

# Verificación end-to-end de los 41 escaneos reales + benchmark de estabilidad de RAM (VmRSS) y exports PNG/TIFF/JPEG
cargo run --release -p photocut-core --example stress_and_verify -- data-photo tmp/final_test_export

# Diagnóstico visual de overlays y muestras de color
cargo run --release -p photocut-core --example verify_dataset -- data-photo tmp/verify

# Prueba puntual de auto-orientación ONNX
cargo run --release -p photocut-core --example verify_orient
```

## Compilación para Linux y Windows

### Linux (Debian / Ubuntu)

Instalar dependencias de ventana/diálogos para `eframe` y `rfd`:

```bash
sudo apt-get update && sudo apt-get install -y \
  pkg-config libgtk-3-dev libxcb-render0-dev libxcb-shape0-dev \
  libxcb-xfixes0-dev libxkbcommon-dev libgl1-mesa-dev

cargo build --release -p photocut-app
# Binario generado: target/release/photocut-app
```

### Windows (MSVC / PowerShell)

Requiere Rust estable (`rustup`, toolchain `x86_64-pc-windows-msvc` con Visual Studio Build Tools):

```powershell
cargo build --release -p photocut-app
# Binario generado: target\release\photocut-app.exe
```

## Estructura del repositorio

```text
photocut/
├── .github/workflows/ci.yml   # CI multiplataforma (Ubuntu + Windows)
├── Cargo.toml                 # Workspace de Rust
├── crates/
│   ├── photocut-core/         # Motor puro en Rust (detect, enhance, export, cache, session, orient)
│   └── photocut-app/          # Interfaz de escritorio egui/eframe + worker en segundo plano
└── docs/
    └── PLAN.md                # Arquitectura y análisis del dataset
```
