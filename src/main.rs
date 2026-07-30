#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use codex_usage_monitor::{app::MonitorApp, platform};

fn main() -> eframe::Result {
    platform::install_panic_hook();

    let viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("com.NHNDeu.CodexUsageMonitor")
        .with_title("Codex 额度监控")
        .with_inner_size([980.0, 720.0])
        .with_min_inner_size([760.0, 520.0])
        .with_icon(platform::app_icon());

    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        persist_window: true,
        run_and_return: false,
        ..Default::default()
    };

    eframe::run_native(
        "Codex Usage Monitor",
        options,
        Box::new(|cc| Ok(Box::new(MonitorApp::new(cc)))),
    )
}
