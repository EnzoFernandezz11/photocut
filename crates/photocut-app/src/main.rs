use photocut_core::{Adjustments, ExportFormat, QuarterTurn};

fn main() {
    // Placeholder mínimo hasta implementar la interfaz gráfica en Fase 2.
    let defaults = Adjustments::default();
    println!(
        "PhotoCut v{} — motor listo (auto_color={:.0}%, rotación={:?}, export={:?})",
        env!("CARGO_PKG_VERSION"),
        defaults.auto_color_strength * 100.0,
        QuarterTurn::default(),
        ExportFormat::default(),
    );
}
