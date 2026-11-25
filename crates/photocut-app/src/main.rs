//! PhotoCut — aplicación de escritorio para recorte automático y restauración de fotos escaneadas.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod crop_editor;
mod icons;
mod worker;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

use crate::app::{CliArgs, PhotoCutApp};

/// Paleta neutral de cuarto oscuro (Operate mode) y configuración visual de `egui`.
pub(crate) mod theme {
    use eframe::egui::{
        self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals,
    };

    pub const BG_VIEWPORT: Color32 = Color32::from_rgb(0x12, 0x13, 0x16);
    pub const BG_SURFACE: Color32 = Color32::from_rgb(0x1B, 0x1D, 0x22);
    pub const BG_CARD: Color32 = Color32::from_rgb(0x24, 0x27, 0x2F);
    pub const BG_CARD_HOVER: Color32 = Color32::from_rgb(0x2B, 0x2F, 0x3A);
    pub const BORDER: Color32 = Color32::from_rgb(0x2E, 0x32, 0x3C);

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xEC, 0xEE, 0xF2);
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x96, 0x9C, 0xA8);

    pub const ACCENT: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x60, 0xA5, 0xFA);
    pub const APPROVED: Color32 = Color32::from_rgb(0x22, 0xC5, 0x5E);
    pub const RECROP: Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B);
    pub const ERROR: Color32 = Color32::from_rgb(0xEF, 0x44, 0x44);
    pub const SKIPPED: Color32 = Color32::from_rgb(0x6B, 0x72, 0x80);

    pub const fn stroke(width: f32, color: Color32) -> Stroke {
        Stroke { width, color }
    }

    pub fn apply_darkroom_theme(ctx: &egui::Context) {
        let mut visuals = Visuals::dark();
        visuals.override_text_color = Some(TEXT_PRIMARY);
        visuals.panel_fill = BG_SURFACE;
        visuals.window_fill = BG_SURFACE;
        visuals.extreme_bg_color = BG_VIEWPORT;
        visuals.faint_bg_color = BG_CARD;
        visuals.window_stroke = stroke(1.0, BORDER);
        visuals.window_corner_radius = CornerRadius::same(6);

        visuals.selection.bg_fill = ACCENT;
        visuals.selection.stroke = stroke(1.0, Color32::WHITE);

        visuals.widgets.noninteractive.bg_fill = BG_SURFACE;
        visuals.widgets.noninteractive.weak_bg_fill = BG_CARD;
        visuals.widgets.noninteractive.bg_stroke = stroke(1.0, BORDER);
        visuals.widgets.noninteractive.fg_stroke = stroke(1.0, TEXT_SECONDARY);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(5);

        visuals.widgets.inactive.bg_fill = BG_CARD;
        visuals.widgets.inactive.weak_bg_fill = BG_CARD;
        visuals.widgets.inactive.bg_stroke = stroke(1.0, BORDER);
        visuals.widgets.inactive.fg_stroke = stroke(1.0, TEXT_PRIMARY);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(5);

        visuals.widgets.hovered.bg_fill = BG_CARD_HOVER;
        visuals.widgets.hovered.weak_bg_fill = BG_CARD_HOVER;
        visuals.widgets.hovered.bg_stroke = stroke(1.0, ACCENT_HOVER);
        visuals.widgets.hovered.fg_stroke = stroke(1.2, TEXT_PRIMARY);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(5);

        visuals.widgets.active.bg_fill = ACCENT;
        visuals.widgets.active.weak_bg_fill = ACCENT;
        visuals.widgets.active.bg_stroke = stroke(1.0, ACCENT_HOVER);
        visuals.widgets.active.fg_stroke = stroke(1.5, Color32::WHITE);
        visuals.widgets.active.corner_radius = CornerRadius::same(5);

        visuals.widgets.open.bg_fill = BG_CARD_HOVER;
        visuals.widgets.open.weak_bg_fill = BG_CARD_HOVER;
        visuals.widgets.open.bg_stroke = stroke(1.0, ACCENT);
        visuals.widgets.open.fg_stroke = stroke(1.0, TEXT_PRIMARY);
        visuals.widgets.open.corner_radius = CornerRadius::same(5);

        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.slider_width = 160.0;
        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(13.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(12.5, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(11.5, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(17.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(12.0, FontFamily::Monospace),
        );
        ctx.set_style(style);
    }
}

fn parse_cli_args<I: IntoIterator<Item = String>>(args: I) -> CliArgs {
    let mut cli = CliArgs::default();
    let mut iter = args.into_iter().skip(1);

    while let Some(arg) = iter.next() {
        if arg == "--input" {
            if let Some(val) = iter.next() {
                cli.input_dir = Some(PathBuf::from(val));
            }
        } else if let Some(val) = arg.strip_prefix("--input=") {
            cli.input_dir = Some(PathBuf::from(val));
        } else if arg == "--output" {
            if let Some(val) = iter.next() {
                cli.output_dir = Some(PathBuf::from(val));
            }
        } else if let Some(val) = arg.strip_prefix("--output=") {
            cli.output_dir = Some(PathBuf::from(val));
        }
    }

    cli
}

const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon.png");

fn load_app_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(APP_ICON_PNG).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

const RENDERER_FLAG: &str = "--renderer=";

/// Appends a line to `photocut-error.log` next to the exe (or in the temp dir if not writable).
fn append_log(msg: &str) -> PathBuf {
    use std::io::Write;
    let line = format!("[PhotoCut v{}] {msg}\n", env!("CARGO_PKG_VERSION"));
    let exe_dir_log = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("photocut-error.log")));
    for path in exe_dir_log
        .into_iter()
        .chain([env::temp_dir().join("photocut-error.log")])
    {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path)
            && f.write_all(line.as_bytes()).is_ok()
        {
            return path;
        }
    }
    env::temp_dir().join("photocut-error.log")
}

fn show_fatal(msg: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("PhotoCut no pudo iniciar")
        .set_description(msg)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

fn main() {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        append_log(&format!("PANIC: {info}"));
        prev_hook(info);
    }));

    let args: Vec<String> = env::args().collect();
    let cli = parse_cli_args(args.clone());

    // Windows: DirectX 12 via wgpu works on almost every PC (even without GPU drivers, via WARP).
    // Other OS: OpenGL. Each falls back to the other one.
    let forced = args
        .iter()
        .find_map(|a| a.strip_prefix(RENDERER_FLAG).map(str::to_owned));
    let use_wgpu = match forced.as_deref() {
        Some("wgpu") => true,
        Some(_) => false,
        None => cfg!(windows),
    };
    let (renderer, name, fallback) = if use_wgpu {
        (eframe::Renderer::Wgpu, "wgpu", "glow")
    } else {
        (eframe::Renderer::Glow, "glow", "wgpu")
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("PhotoCut v{}", env!("CARGO_PKG_VERSION")))
        .with_inner_size([1360.0, 860.0])
        .with_min_inner_size([960.0, 640.0]);
    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer,
        ..Default::default()
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native(
            "PhotoCut",
            options,
            Box::new(move |cc| {
                theme::apply_darkroom_theme(&cc.egui_ctx);
                Ok(Box::new(PhotoCutApp::new(cc.egui_ctx.clone(), cli)))
            }),
        )
    }));

    let err = match result {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e.to_string(),
        Err(_) => "la aplicación se cerró inesperadamente (ver detalle en el log)".to_string(),
    };
    let log_path = append_log(&format!("renderer {name} failed: {err}"));

    // winit cannot recreate its event loop in-process, so retry the other renderer in a fresh process.
    if forced.is_none()
        && let Ok(exe) = env::current_exe()
    {
        let mut retry_args: Vec<String> = args.iter().skip(1).cloned().collect();
        retry_args.push(format!("{RENDERER_FLAG}{fallback}"));
        if std::process::Command::new(exe).args(&retry_args).spawn().is_ok() {
            return;
        }
    }

    show_fatal(&format!(
        "No se pudo abrir la ventana de PhotoCut.\n\nDetalle: {err}\n\n\
         Se guardó un registro en:\n{}\n\n\
         Probá actualizar los drivers de video de la PC y volver a abrir PhotoCut.",
        log_path.display()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_input_and_output_cli_flags() {
        let cli = parse_cli_args([
            "photocut-app".to_string(),
            "--input".to_string(),
            "/scans/album".to_string(),
            "--output=/out/album".to_string(),
        ]);
        assert_eq!(cli.input_dir, Some(PathBuf::from("/scans/album")));
        assert_eq!(cli.output_dir, Some(PathBuf::from("/out/album")));
    }

    #[test]
    fn parses_empty_cli_args() {
        let cli = parse_cli_args(["photocut-app".to_string()]);
        assert_eq!(cli.input_dir, None);
        assert_eq!(cli.output_dir, None);
    }

    #[test]
    fn embedded_app_icon_decodes_cleanly() {
        let icon = load_app_icon().expect("embedded icon.png should decode");
        assert_eq!(icon.width, 256);
        assert_eq!(icon.height, 256);
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
