#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod disk;
mod engine;
mod fonts;
mod ui;

use ui::MyDiskTestApp;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "--raw-probe-worker" {
        let code = engine::raw_probe::run_cli_worker(&args[2..]);
        std::process::exit(code);
    }
    if args.len() >= 2 && args[1] == "--error-scan-worker" {
        let code = engine::error_scan::run_cli_worker(&args[2..]);
        std::process::exit(code);
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("MyDiskTest Next — 跨平台磁盘扩容与性能检测工具")
        .with_inner_size([1180.0, 780.0])
        .with_min_inner_size([920.0, 620.0]);

    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon_256.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "MyDiskTest Next",
        native_options,
        Box::new(|cc| Ok(Box::new(MyDiskTestApp::new(cc)))),
    )
}
