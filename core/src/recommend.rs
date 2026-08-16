//! 判定支援ロジック（推奨の動的補正）。F-REC-04〜08。

use crate::entry::ScanEntry;
use crate::rule::Rule;

/// [`recommend`] の戻り値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    /// 推奨可否。
    pub recommended: bool,
    /// 推奨・非推奨の理由（短文）。
    pub reason: String,
}

/// `entry` と `rule` を入力に、状況に応じた推奨可否と理由を返す純粋関数。
///
/// 契約: I/O を行わない・現在時刻を参照しない（`age_days` は呼び出し側が
/// 走査時に算出済みの値を渡す）。これにより単体テスト可能かつ CLI/GUI で
/// 同一に作用することを保証する（F-REC-05）。
///
/// # Panics
///
/// 本実装は #6 でのプレースホルダであり、呼び出すと必ず panic する。
pub fn recommend(entry: &ScanEntry, rule: &Rule) -> Recommendation {
    let _ = (entry, rule);
    todo!("#6 で実装する")
}
