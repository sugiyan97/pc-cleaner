//! pc-cleaner の GUI（egui、F-GUI-01〜07）。
//!
//! 判定・削除ロジックは持たず、`pc-cleaner-core` の呼び出しと表示・操作のみ
//! を行う（F-GUI-07 / NF-MNT-01）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_code)]

mod app;
#[cfg(feature = "demo")]
mod demo;
mod fonts;
mod task;
mod view;

use app::App;

fn main() -> eframe::Result<()> {
    let demo = std::env::args().any(|a| a == "--demo");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([800.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "pc-cleaner",
        options,
        Box::new(move |cc| {
            if !fonts::install_japanese_font(&cc.egui_ctx) {
                eprintln!("日本語フォントが見つかりませんでした。表示が崩れる場合があります。");
            }
            Ok(Box::new(App::new(demo)))
        }),
    )
}
