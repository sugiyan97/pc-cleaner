//! 走査結果として得られた削除候補エントリ。

use std::path::PathBuf;
use std::time::SystemTime;

/// 重複ファイルグループでのこのエントリの位置づけ（C2 / Issue #48）。
///
/// あくまで情報提供のためのものであり、`recommend()` の推奨可否そのものを
/// 変えることはない（重複していても安全に削除できるとは限らないため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateInfo {
    /// このスキャン内で一意なグループ識別子。
    pub group_id: u64,
    /// グループに属するエントリ数（2以上）。
    pub group_size: usize,
    /// このグループの「代表」（最も古い＝残す候補）かどうか。
    pub is_primary: bool,
}

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
    /// 他プロセスに使用中と判定されたか（C2 / Issue #48）。
    ///
    /// `age_days` と同じ理由で、走査時（`inspect::annotate_in_use`）に
    /// あらかじめ算出して格納する独立フィールドとして持つ。判定には
    /// ファイルを開く等の I/O が必要であり、[`crate::recommend::recommend`]
    /// を I/O 非依存の純粋関数に保つため（NF-MNT-02 / F-REC-05）。
    /// `file_count != 1`（集約されたディレクトリ）では判定を行わず `None`
    /// のままにする。判定できない場合も `None`（「使用中でない」と断定
    /// しない）。
    pub in_use: Option<bool>,
    /// 重複ファイルグループの情報（C2 / Issue #48）。`Config::detect_duplicates`
    /// が `false`、または対象外（`file_count != 1` 等）の場合は `None`。
    pub duplicate: Option<DuplicateInfo>,
    /// ツールの推奨可否（初期チェック状態）。
    pub recommended: bool,
    /// 推奨・非推奨の理由（短文）。
    pub reason: String,
    /// ユーザーの選択状態。
    pub selected: bool,
}
