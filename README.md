# PhotoCut

Aplicación de escritorio multiplataforma (**Windows y Linux**) para separar, enderezar, restaurar color/luminosidad y exportar en máxima calidad fotos individuales contenidas dentro de escaneos múltiples, con revisión y re-recorte interactivo por un humano.

## Estructura del repositorio

```text
photocut/
├── Cargo.toml                 # Workspace de Rust
├── .gitignore                 # Excluye data-photo/, exports/, target/ y modelos pesados
├── docs/
│   └── PLAN.md                # Arquitectura, análisis de muestras y plan por fases
└── crates/
    ├── photocut-core/         # Motor puro en Rust (detección, enderezado, color, export)
    └── photocut-app/          # Interfaz de escritorio (Windows + Linux)
```

> **Fotos de muestra (`data-photo/`):** están excluidas en `.gitignore` para no subir cientos de MB al repositorio. Se usan localmente para probar y calibrar el algoritmo de recorte.

## Comandos básicos

```bash
# Verificar compilación y tests del núcleo
cargo test

# Ejecutar la app
cargo run -p photocut-app
```

Consultá [docs/PLAN.md](docs/PLAN.md) para ver el diseño técnico completo y las fases de desarrollo.
