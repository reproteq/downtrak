// Oculta la consola en Windows en modo release
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod clean;
mod tools;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("YT → MP3 · Descargar y cortar")
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([720.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "yt_mp3_gui",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
