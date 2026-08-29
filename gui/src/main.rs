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
    // A2 / Issue #41: 管理者権限で起動し直された後のプロセスであることを
    // 示す内部用マーカー。利用者が指定するものではない。
    let relaunched = std::env::args().any(|a| a == "--elevated");

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
            // 既定の暗いテーマは文字と背景のコントラストが弱く読みづらいため、
            // 明るいテーマを既定にする（F-GUI-01 の視認性）。
            let mut visuals = egui::Visuals::light();
            // Visuals::light() は非ホバー時のボタン／チェックボックス／
            // コンボボックス（inactive）に枠線を持たない（bg_stroke が
            // 既定値のまま）ため、背景色との差が薄いパネル上では輪郭が
            // 分かりづらい。常に薄いグレーの枠を付けて境界を分かりやすくする。
            visuals.widgets.inactive.bg_stroke =
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(170));
            cc.egui_ctx.set_visuals(visuals);
            if !fonts::install_japanese_font(&cc.egui_ctx) {
                eprintln!("日本語フォントが見つかりませんでした。表示が崩れる場合があります。");
            }
            Ok(Box::new(App::new(demo, relaunched)))
        }),
    )
}
