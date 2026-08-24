//! 日本語フォントの読み込み。
//!
//! `egui` の既定フォントには CJK グリフが含まれないため、システムに
//! インストールされている日本語フォントを探して差し込む。見つからない
//! 場合は既定フォントのまま起動を続ける（起動を妨げない。`config` の
//! 読込フォールバックと同じ思想）。OS 固有のフォント探索は `fontdb`
//! 内部に閉じており、この関数自体に `#[cfg]` は登場しない。
//!
//! フォントごとの縦位置補正（[`FontTweak::y_offset_factor`]）は、CJK
//! フォント間で ascent/descent が大きく異なり実機で個別に実測しないと
//! 正しい値が分からないため、`CANDIDATE_FAMILIES` でフォント名ごとに
//! 個別の値を持たせている（詳細は下記コメントおよび issue #34 参照）。

use egui::{FontData, FontDefinitions, FontFamily, FontTweak};
use std::borrow::Cow;
use std::sync::Arc;

/// 縦位置補正なし（既定値）。実機で測っていないフォントに対しては、
/// 誤った補正を当てるより無補正の方が安全という判断で使う。
const NO_TWEAK: FontTweak = FontTweak {
    scale: 1.0,
    y_offset_factor: 0.0,
    y_offset: 0.0,
    baseline_offset_factor: 0.0,
};

// CJK フォントは既定の欧文フォントと ascent/descent が異なるため、
// 補正なしでは日本語のグリフが行内で上に寄る。実機（Mac, Retina）の
// スクリーンショットを ImageMagick で実測したところ、コンボボックスの
// ラベルも行内のラベルも、egui がベクタ描画する（＝正しく中央にある）
// チェックマークや▼アイコンの中心より約 6px（Retina 2x のため論理 3pt）
// 上にずれていた。既定の本文サイズ 14pt に対して 3/14 ≒ 0.21 なので
// 0.2 を下方向（正の値）に与える。
//
// baseline_offset_factor ではなく y_offset_factor を使うのは、前者が
// このフォントの行レイアウト（行高・行間）そのものに影響し、ずれの
// 報告がない箇所（各行の下のパス／理由の行など）まで動かしてしまう
// ため。epaint のドキュメント通り y_offset_factor は「見た目だけを
// 動かしテキストレイアウトには影響しない」ので、同じ行内の兄弟
// ウィジェットに対する描画位置のずれという今回の症状に対して副作用が
// 小さい。以前試した baseline_offset_factor: -0.2 は補正方向が逆
// （さらに上へ）で悪化させていた。
//
// この値は Hiragino Sans（macOS）専用にチューニングされたもので、
// ascent/descent の異なる他の CJK フォントにそのまま適用してよい保証は
// ない。実際、Windows で優先的にマッチする Yu Gothic UI / Meiryo /
// MS Gothic 等にも一律にこの値を適用していたことが、issue #34
// （「Windows のUIがずれている。Macビルド時と異なる」）の主因と見られる。
// そのため、この値は Hiragino 系にのみ与え、他のフォントは実機で計測
// できるまで `NO_TWEAK`（無補正）とする。
const HIRAGINO_TWEAK: FontTweak = FontTweak {
    scale: 1.0,
    y_offset_factor: 0.2,
    y_offset: 0.0,
    baseline_offset_factor: 0.0,
};

const CANDIDATE_FAMILIES: &[(&str, FontTweak)] = &[
    ("Hiragino Sans", HIRAGINO_TWEAK),
    ("Hiragino Kaku Gothic ProN", HIRAGINO_TWEAK),
    ("Yu Gothic UI", NO_TWEAK),
    ("Yu Gothic", NO_TWEAK),
    ("Meiryo", NO_TWEAK),
    ("MS Gothic", NO_TWEAK),
    ("Noto Sans CJK JP", NO_TWEAK),
    ("Noto Sans JP", NO_TWEAK),
];

/// システムの日本語フォントを探して `ctx` に登録する。登録できたら
/// `true` を返す。
pub fn install_japanese_font(ctx: &egui::Context) -> bool {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();

    for (family, tweak) in CANDIDATE_FAMILIES {
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
            tweak: *tweak,
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
