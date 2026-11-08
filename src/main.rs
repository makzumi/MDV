// Release builds on Windows: no console window behind the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod doc;
mod editor;
mod find;
mod fonts;
mod images;
mod live;
mod markdown;
mod ops;
mod theme;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result {
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    #[allow(unused_mut)]
    let mut size = [1100.0, 800.0];
    // Debug builds only: MDV_DEV_SIZE=600x700 sets the starting window size.
    #[cfg(debug_assertions)]
    if let Some((w, h)) = std::env::var("MDV_DEV_SIZE").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    }) {
        size = [w, h];
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MDV")
            .with_app_id("mdv")
            .with_inner_size(size)
            .with_min_inner_size([420.0, 300.0])
            .with_drag_and_drop(true)
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/mdv.png")).unwrap_or_default()),
        ..Default::default()
    };

    eframe::run_native("mdv", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, path)))))
}
