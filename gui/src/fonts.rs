//! 日本語フォントの読み込み。
//!
//! `egui` の既定フォントには CJK グリフが含まれないため、システムに
//! インストールされている日本語フォントを探して差し込む。見つからない
//! 場合は既定フォントのまま起動を続ける（起動を妨げない。`config` の
//! 読込フォールバックと同じ思想）。OS 固有のフォント探索は `fontdb`
//! 内部に閉じており、この関数自体に `#[cfg]` は登場しない。

use egui::{FontData, FontDefinitions, FontFamily, FontTweak};
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
            // CJK フォントは既定の欧文フォントと ascent/descent が異なるため、
            // 補正なしでは日本語のグリフが行内で上に寄る。実機のスクリーン
            // ショットを ImageMagick で実測したところ、コンボボックスのラベル
            // も行内のラベルも、egui がベクタ描画する（＝正しく中央にある）
            // チェックマークや▼アイコンの中心より約 6px（Retina 2x のため
            // 論理 3pt）上にずれていた。既定の本文サイズ 14pt に対して
            // 3/14 ≒ 0.21 なので 0.2 を下方向（正の値）に与える。
            //
            // baseline_offset_factor ではなく y_offset_factor を使うのは、
            // 前者がこのフォントの行レイアウト（行高・行間）そのものに影響し、
            // ずれの報告がない箇所（各行の下のパス／理由の行など）まで動かして
            // しまうため。epaint のドキュメント通り y_offset_factor は
            // 「見た目だけを動かしテキストレイアウトには影響しない」ので、
            // 同じ行内の兄弟ウィジェットに対する描画位置のずれという今回の
            // 症状に対して副作用が小さい。以前試した baseline_offset_factor:
            // -0.2 は補正方向が逆（さらに上へ）で悪化させていた。
            tweak: FontTweak {
                scale: 1.0,
                y_offset_factor: 0.2,
                y_offset: 0.0,
                baseline_offset_factor: 0.0,
            },
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
