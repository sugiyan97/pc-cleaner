//! 掃除ルール（許可リストの1項目）。判定支援の一次情報。

use crate::platform::KnownDir;

/// ルールの安全度区分。既定の選択状態と表示方法を決定する（5.2.1）。
///
/// variant の宣言順はリスクの昇順（`Safe` が最も安全）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Safety {
    /// 再生成される一時領域・キャッシュ。既定でチェック ON。
    Safe,
    /// 状況次第（古いダウンロード、大きなログ等）。既定 OFF・注意色で表示。
    Caution,
    /// 中身の確認が必要。既定 OFF・確認前提。
    Review,
}

/// ルール内での対象絞り込み方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchKind {
    /// 基点配下の全件を対象とする。
    All,
    /// 指定拡張子のみを対象とする。
    ///
    /// 拡張子は先頭ドットなし・小文字で保持する（例: `"log"`, `"tmp"`）。
    Extension(Vec<String>),
    /// `Rule::age_threshold_days` と併用し、経過日数で絞り込む。
    OlderThan,
}

/// 掃除ルール。許可リストの1項目であり、判定支援の一次情報。
///
/// 走査の基点・絞り込み条件・安全度・説明文などを保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// 安定した識別子（例: `"user_temp"`）。
    pub id: String,
    /// 表示名（例: 「ユーザー一時ファイル」）。
    pub label: String,
    /// このルールが何を・なぜ対象とするかの説明文（F-REC-01）。
    pub description: String,
    /// 走査の基点（抽象キー）。
    pub base: KnownDir,
    /// 対象の絞り込み方式。
    pub match_kind: MatchKind,
    /// 削除に管理者権限が要るか。`true` のルールは初版ではフィルタ除外する（F-SCAN-04）。
    pub needs_admin: bool,
    /// 安全度区分。
    pub safety: Safety,
    /// 推奨判定に使う経過日数の既定しきい値。
    ///
    /// 初版ではルールにハードコードするが、将来 `Config` へ移設できるよう
    /// `Rule` 本体とは独立したフィールドとして分離しておく（NF-EXT-03 / 将来対応 C1）。
    pub age_threshold_days: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_ordered_by_risk_ascending() {
        assert!(Safety::Safe < Safety::Caution);
        assert!(Safety::Caution < Safety::Review);
    }
}
