//! UI 言語設定（E4 / Issue #58）。
//!
//! ここに置くのは言語の識別子だけである。実際の文言は、その文言を生成する
//! 関数（`rule::builtin_rules` / `recommend::recommend` / `format::relative_days`
//! / gui の各表示ヘルパー）がそれぞれ `Lang` を受け取って内部で
//! `match` する（中央集権的な翻訳テーブルは持たない。文言とそれを生成する
//! ロジックを同じ場所に置き、片方を直しても他方が古いまま残る事故を防ぐ
//! ため）。

/// UI 表示言語。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Lang {
    /// 日本語（既定）。
    #[default]
    Ja,
    /// 英語。
    En,
}
