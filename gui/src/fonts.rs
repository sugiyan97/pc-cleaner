//! 日本語フォントの読み込み。
//!
//! `egui` の既定フォントには CJK グリフが含まれないため、システムに
//! インストールされている日本語フォントを探して差し込む。見つからない
//! 場合は既定フォントのまま起動を続ける（起動を妨げない。`config` の
//! 読込フォールバックと同じ思想）。OS 固有のフォント探索は `fontdb`
//! 内部に閉じており、この関数自体に `#[cfg]` は登場しない。

use egui::{FontData, FontDefinitions, FontFamily};
use std::borrow::Cow;
use std::sync::Arc;

const CANDIDATE_FAMILIES: &[&str] = &[
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "Yu Gothic UI",
    "Yu Gothic",
    "Meiryo",
    "MS Gothic",
    "Noto Sans CJK JP",
    "Noto Sans JP",
];

/// システムの日本語フォントを探して `ctx` に登録する。登録できたら
/// `true` を返す。
pub fn install_japanese_font(ctx: &egui::Context) -> bool {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();

    for family in CANDIDATE_FAMILIES {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            ..Default::default()
        };
        let Some(id) = db.query(&query) else {
            continue;
        };
        let font_data = db.with_face_data(id, |bytes, index| FontData {
            font: Cow::Owned(bytes.to_vec()),
            index,
            tweak: Default::default(),
        });
        let Some(font_data) = font_data else {
            continue;
        };

        let mut fonts = FontDefinitions::default();
        fonts
            .font_data
            .insert("japanese".to_string(), Arc::new(font_data));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "japanese".to_string());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push("japanese".to_string());
        ctx.set_fonts(fonts);
        return true;
    }
    false
}
