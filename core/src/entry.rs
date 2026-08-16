//! 走査結果として得られた削除候補エントリ。

use std::path::PathBuf;
use std::time::SystemTime;

/// 走査結果の1候補。「一覧だけでは判定できない」への回答となるメタ情報を持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntry {
    /// 由来ルールの `Rule::id`。
    pub rule_id: String,
    /// 対象のパス。
    pub path: PathBuf,
    /// サイズ（バイト単位）。
    pub size: u64,
    /// 基点配下の実ファイル数。
    pub file_count: u64,
    /// 最終更新日時。取得できない場合は `None`。
    pub modified: Option<SystemTime>,
    /// 経過日数。判定支援の中心となる値（F-REC-03）。
    ///
    /// `modified` から導出可能だが、あえて独立フィールドとして保持する。
    /// 導出には「現在時刻」という外部入力が必要であり、走査時（#5）に
    /// 算出して格納することで、[`crate::recommend::recommend`] を
    /// I/O・時刻非依存の純粋関数に保てる（NF-MNT-02 / F-REC-05）。
    pub age_days: Option<u64>,
    /// ツールの推奨可否（初期チェック状態）。
    pub recommended: bool,
    /// 推奨・非推奨の理由（短文）。
    pub reason: String,
    /// ユーザーの選択状態。
    pub selected: bool,
}
