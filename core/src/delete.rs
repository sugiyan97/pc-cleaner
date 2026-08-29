//! 削除実行（delete）。「走査 → プレビュー → 実行」の3段を型で強制する。
//! F-DEL-01〜06。誤削除の防止が最優先事項（NF-SAF-01）。
//!
//! [`DeletePlan`] はフィールドを一切公開せず、[`preview`] 以外に生成手段を
//! 持たない。[`execute`] / [`execute_with_progress`] は `DeletePlan` を値で
//! 受け取って消費するため、同じ計画を経由せずに削除を実行する経路も、同じ
//! 計画を二重実行する経路も存在しない（F-DEL-01）。
//!
//! `recycle_bin` ルール（`base == KnownDir::RecycleBin`）由来のエントリは
//! `Platform::to_trash` の対象にしない。走査対象は既にゴミ箱の中にあり、
//! ゴミ箱へ送る操作は意味をなさないため。かわりに、完全削除
//! （[`DeleteMethod::Permanent`]、`--permanent` 明示時のみ）でのみ実行し、
//! コンテナ（SID ディレクトリ）自体は残して中身だけを恒久削除する
//! （[`DeleteAction::EmptyContainer`]）。ゴミ箱を空にする操作は復旧不可能な
//! ため、通常の削除フロー（ゴミ箱経由・既定）には混入させず、F-DEL-06 の
//! 確認ゲート（`--permanent` 明示＋確認）をそのまま利用する（Issue #17）。

use crate::config::Config;
use crate::entry::ScanEntry;
use crate::platform::{KnownDir, Platform};
use crate::rule::Rule;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------
// モード決定
// ---------------------------------------------------------------------

/// UI からの削除指示。名前付きコンストラクタ経由でのみ生成できる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteRequest {
    kind: RequestKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    FromConfig,
    DryRun,
    Execute,
    PermanentConfirmed,
    PermanentUnconfirmed,
}

impl DeleteRequest {
    /// 設定の既定（`Config::dry_run_default` / `Config::use_trash`）に従う。
    pub fn from_config() -> Self {
        DeleteRequest {
            kind: RequestKind::FromConfig,
        }
    }

    /// ドライランを明示する（例：CLI の `scan` / `clean --dry-run`）。
    pub fn dry_run() -> Self {
        DeleteRequest {
            kind: RequestKind::DryRun,
        }
    }

    /// 明示実行する（例：CLI の `clean`）。`Config::use_trash` が `false` の
    /// 場合は完全削除に昇格せず、理由付きでドライランへフォールバックする。
    pub fn execute() -> Self {
        DeleteRequest {
            kind: RequestKind::Execute,
        }
    }

    /// 完全削除を明示指定する。復旧不可能である旨を UI がユーザーに提示し、
    /// 同意を得たときにのみ呼ぶこと（F-DEL-06）。
    pub fn permanent_confirmed() -> Self {
        DeleteRequest {
            kind: RequestKind::PermanentConfirmed,
        }
    }

    /// 完全削除を要求されたが、確認が取れていない状態。安全側に倒し
    /// ドライランとして扱う。
    pub fn permanent_unconfirmed() -> Self {
        DeleteRequest {
            kind: RequestKind::PermanentUnconfirmed,
        }
    }
}

/// 削除の実行方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMethod {
    /// `Platform::to_trash` によるゴミ箱送り（復旧可能、F-DEL-02）。
    Trash,
    /// パスの恒久削除（復旧不可、F-DEL-06）。
    Permanent,
}

/// ドライランになった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRunReason {
    /// `Config::dry_run_default` が `true` だった。
    ConfigDefault,
    /// 呼び出し側がドライランを明示した。
    ExplicitRequest,
    /// 完全削除が要求されたが確認が取れていない。
    PermanentNotConfirmed,
    /// `Config::use_trash` が `false` のため、ゴミ箱送りを実行できない
    /// （完全削除への自動昇格はしない。F-DEL-06 は明示オプションを要求する）。
    TrashDisabled,
}

/// 解決済みの実行モード。[`DeleteMode::resolve`] 以外に生成手段を持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteMode {
    dry_run_reason: Option<DryRunReason>,
    method: DeleteMethod,
}

impl DeleteMode {
    /// `config` と `request` から実行モードを決定する純粋関数（I/O なし）。
    ///
    /// `DeleteMethod::Permanent` を生成しうるのは
    /// [`DeleteRequest::permanent_confirmed`] /
    /// [`DeleteRequest::permanent_unconfirmed`] の2経路のみであり、
    /// 実際にドライランでない完全削除になるのは
    /// `permanent_confirmed()` のときだけである（F-DEL-06）。
    pub fn resolve(config: &Config, request: DeleteRequest) -> Self {
        match request.kind {
            RequestKind::FromConfig => {
                if config.dry_run_default {
                    DeleteMode {
                        dry_run_reason: Some(DryRunReason::ConfigDefault),
                        method: DeleteMethod::Trash,
                    }
                } else {
                    Self::trash_or_disabled(config)
                }
            }
            RequestKind::DryRun => DeleteMode {
                dry_run_reason: Some(DryRunReason::ExplicitRequest),
                method: DeleteMethod::Trash,
            },
            RequestKind::Execute => Self::trash_or_disabled(config),
            RequestKind::PermanentConfirmed => DeleteMode {
                dry_run_reason: None,
                method: DeleteMethod::Permanent,
            },
            RequestKind::PermanentUnconfirmed => DeleteMode {
                dry_run_reason: Some(DryRunReason::PermanentNotConfirmed),
                method: DeleteMethod::Permanent,
            },
        }
    }

    fn trash_or_disabled(config: &Config) -> Self {
        if config.use_trash {
            DeleteMode {
                dry_run_reason: None,
                method: DeleteMethod::Trash,
            }
        } else {
            DeleteMode {
                dry_run_reason: Some(DryRunReason::TrashDisabled),
                method: DeleteMethod::Trash,
            }
        }
    }

    /// ドライランかどうか。
    pub fn is_dry_run(&self) -> bool {
        self.dry_run_reason.is_some()
    }

    /// 実行方法（ドライランでも、実行されたなら使われたはずの方法を返す）。
    pub fn method(&self) -> DeleteMethod {
        self.method
    }

    /// ドライランになった理由。ドライランでない場合は `None`。
    pub fn dry_run_reason(&self) -> Option<DryRunReason> {
        self.dry_run_reason
    }
}

// ---------------------------------------------------------------------
// プレビュー
// ---------------------------------------------------------------------

/// 項目ごとに実際に行う削除操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteAction {
    /// `Platform::to_trash` でゴミ箱へ送る。
    ToTrash,
    /// パスごと恒久削除する。
    RemovePath,
    /// コンテナ自体は残し、直下の子だけを恒久削除する（ゴミ箱を空にする、#17）。
    EmptyContainer,
}

/// 削除が計画された1項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDeletion {
    /// 由来ルールの `id`。
    pub rule_id: String,
    /// 削除対象のパス。
    pub path: PathBuf,
    /// サイズ（バイト）。
    pub size: u64,
    /// ファイル数。
    pub file_count: u64,
    /// 実行される操作。
    pub action: DeleteAction,
}

/// プレビュー段で計画から除外された理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionReason {
    /// `ScanEntry::selected` が `false`（F-DEL-03）。
    NotSelected,
    /// `rule_id` が渡された許可リストに存在しない（NF-SAF-04）。
    UnknownRule,
    /// ルールが管理者権限を要する、またはパスが管理者権限領域と判定された（9.5）。
    NeedsAdmin,
    /// パスが由来ルールの基点配下でない、基点そのもの、または `..` を含む。
    OutsideRuleBase,
    /// `Platform::known_dir` が由来ルールの基点を解決できなかった。
    UnresolvedBase,
    /// ゴミ箱由来のエントリだが、完全削除（`--permanent` 明示）が指定されて
    /// いない（Issue #17）。
    RequiresPermanent,
}

/// 計画から除外された1項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedEntry {
    /// 由来ルールの `id`。
    pub rule_id: String,
    /// 除外されたパス。
    pub path: PathBuf,
    /// サイズ（バイト）。
    pub size: u64,
    /// 除外理由。
    pub reason: ExclusionReason,
}

/// 削除計画。[`preview`] 以外に生成手段を持たない（F-DEL-01）。
#[must_use]
#[derive(Debug)]
pub struct DeletePlan {
    mode: DeleteMode,
    items: Vec<PlannedDeletion>,
    excluded: Vec<ExcludedEntry>,
}

impl DeletePlan {
    /// この計画の実行モード。
    pub fn mode(&self) -> &DeleteMode {
        &self.mode
    }

    /// 削除される項目。
    pub fn items(&self) -> &[PlannedDeletion] {
        &self.items
    }

    /// 除外された項目とその理由。
    pub fn excluded(&self) -> &[ExcludedEntry] {
        &self.excluded
    }

    /// 選択分の解放見込み容量。走査時の合計ではなく `items` から再集計した
    /// 値（F-DEL-04）。
    pub fn total_size(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }

    /// 選択分の合計ファイル数。
    pub fn total_file_count(&self) -> u64 {
        self.items.iter().map(|item| item.file_count).sum()
    }

    /// 削除される項目数。
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// 削除される項目が1つもないか。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// `entries` から削除計画を作る。ファイル I/O は行わない
/// （`Platform::known_dir` / `requires_admin` はパス文字列の判定のみ）ため
/// 決定的で、副作用なく何度でも呼べる。
///
/// 安全チェックは以下の順で評価し、最初に該当した理由で除外する：
/// 1. `selected == false`（F-DEL-03）
/// 2. `rules` に `rule_id` が無い（許可リスト方式、NF-SAF-04）
/// 3. ルールが `needs_admin`（9.5 二次防御）
/// 4. `Platform::known_dir` が基点を解決できない
/// 5. パスが基点配下でない・基点そのもの・`..` を含む（許可リスト方式の
///    再検証。`ScanEntry` は pub フィールドで呼び出し側が書き換えうるため、
///    scan 側の保証を信用せず delete 側でも検証する）
/// 6. `Platform::requires_admin(path)` が `true`（9.5 二次防御）
/// 7. ゴミ箱由来で、かつ完全削除でない（Issue #17）
pub fn preview(
    platform: &dyn Platform,
    entries: &[ScanEntry],
    rules: &[Rule],
    mode: DeleteMode,
) -> DeletePlan {
    let mut items = Vec::new();
    let mut excluded = Vec::new();

    for entry in entries {
        match plan_entry(platform, entry, rules, &mode) {
            Ok(planned) => items.push(planned),
            Err(reason) => excluded.push(ExcludedEntry {
                rule_id: entry.rule_id.clone(),
                path: entry.path.clone(),
                size: entry.size,
                reason,
            }),
        }
    }

    DeletePlan {
        mode,
        items,
        excluded,
    }
}

fn plan_entry(
    platform: &dyn Platform,
    entry: &ScanEntry,
    rules: &[Rule],
    mode: &DeleteMode,
) -> Result<PlannedDeletion, ExclusionReason> {
    if !entry.selected {
        return Err(ExclusionReason::NotSelected);
    }

    let rule = rules
        .iter()
        .find(|r| r.id == entry.rule_id)
        .ok_or(ExclusionReason::UnknownRule)?;

    if rule.needs_admin {
        return Err(ExclusionReason::NeedsAdmin);
    }

    let base = platform
        .known_dir(rule.base)
        .ok_or(ExclusionReason::UnresolvedBase)?;

    let contains_parent_dir = entry
        .path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    if contains_parent_dir || entry.path == base || !entry.path.starts_with(&base) {
        return Err(ExclusionReason::OutsideRuleBase);
    }

    if platform.requires_admin(&entry.path) {
        return Err(ExclusionReason::NeedsAdmin);
    }

    let is_trash_container = rule.base == KnownDir::RecycleBin;
    if is_trash_container && mode.method() != DeleteMethod::Permanent {
        return Err(ExclusionReason::RequiresPermanent);
    }

    let action = match (mode.method(), is_trash_container) {
        (DeleteMethod::Trash, _) => DeleteAction::ToTrash,
        (DeleteMethod::Permanent, true) => DeleteAction::EmptyContainer,
        (DeleteMethod::Permanent, false) => DeleteAction::RemovePath,
    };

    Ok(PlannedDeletion {
        rule_id: entry.rule_id.clone(),
        path: entry.path.clone(),
        size: entry.size,
        file_count: entry.file_count,
        action,
    })
}

// ---------------------------------------------------------------------
// 実行
// ---------------------------------------------------------------------

/// 個々の削除項目の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemOutcome {
    /// 削除に成功した。
    Deleted,
    /// ドライランのため削除を試みなかった（F-DEL-05）。
    NotAttempted,
    /// 実行時にパスが既に存在しなかった（`%TEMP%` 等では日常的に起こる。
    /// エラーとしては扱わない）。
    Missing,
    /// 削除に失敗した（権限不足・使用中など）。
    Failed {
        /// エラーメッセージ。
        message: String,
    },
}

/// 実行結果の1項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemResult {
    /// 由来ルールの `id`。
    pub rule_id: String,
    /// 対象のパス。
    pub path: PathBuf,
    /// 計画時点のサイズ（バイト）。
    pub size: u64,
    /// 実行された操作。
    pub action: DeleteAction,
    /// 結果。
    pub outcome: ItemOutcome,
}

/// 削除の進捗イベント。UI 非依存（NF-MNT-01）。常に呼び出し元スレッドから
/// 呼ばれる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteProgress {
    /// 実行を開始した。
    Started {
        /// 削除予定の項目数。
        total_items: usize,
        /// 削除予定の合計サイズ（バイト）。
        total_size: u64,
        /// ドライランかどうか。
        dry_run: bool,
    },
    /// 1項目の処理が完了した。
    ItemFinished {
        /// 対象のパス。
        path: PathBuf,
        /// 結果。
        outcome: ItemOutcome,
        /// この項目で解放されたバイト数（成功時のみサイズ、それ以外は0）。
        freed: u64,
        /// ここまでに処理した項目数。
        done: usize,
        /// 総項目数。
        total: usize,
    },
    /// 全項目の処理が完了した。
    Finished {
        /// 成功件数。
        deleted: usize,
        /// 失敗件数。
        failed: usize,
        /// 解放された合計バイト数（成功分のみ）。
        freed_bytes: u64,
    },
}

/// 実行結果全体。
#[must_use]
#[derive(Debug)]
pub struct DeleteOutcome {
    mode: DeleteMode,
    /// 各項目の結果（計画順）。
    pub results: Vec<ItemResult>,
    /// 計画段階で除外された項目。
    pub excluded: Vec<ExcludedEntry>,
}

impl DeleteOutcome {
    /// 削除に成功した件数。
    pub fn deleted_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome == ItemOutcome::Deleted)
            .count()
    }

    /// 削除に失敗した件数。
    pub fn failed_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, ItemOutcome::Failed { .. }))
            .count()
    }

    /// 実際に解放されたバイト数（成功分のみ。プレビューの見込みとは区別する）。
    pub fn freed_bytes(&self) -> u64 {
        self.results
            .iter()
            .filter(|r| r.outcome == ItemOutcome::Deleted)
            .map(|r| r.size)
            .sum()
    }

    /// ドライランだったか。
    pub fn is_dry_run(&self) -> bool {
        self.mode.is_dry_run()
    }
}

/// `plan` を実行する。進捗通知は行わない。
pub fn execute(platform: &dyn Platform, plan: DeletePlan) -> DeleteOutcome {
    execute_with_progress(platform, plan, |_| {})
}

/// `plan` を実行する。`on_progress` で進捗を通知する。
///
/// ドライランの分岐はここ1箇所だけであり、ドライランのときはファイル
/// システムにも `Platform` にも一切触れない（F-DEL-05）。1件の失敗で
/// 残りの削除を止めない：全項目を試行し、結果を集約して返す。
///
/// 削除は逐次実行する（並列化しない）。`trash` クレートは COM を
/// シングルスレッドアパートメントで初期化しており、並列化すると
/// スレッドごとの初期化が必要になる上、結果順序の決定性（走査 9.5 と
/// 同様の考え方）よりも安全性（NF-SAF-01）を優先する。
pub fn execute_with_progress(
    platform: &dyn Platform,
    plan: DeletePlan,
    mut on_progress: impl FnMut(DeleteProgress),
) -> DeleteOutcome {
    let DeletePlan {
        mode,
        items,
        excluded,
    } = plan;
    let total_items = items.len();
    let total_size: u64 = items.iter().map(|item| item.size).sum();

    on_progress(DeleteProgress::Started {
        total_items,
        total_size,
        dry_run: mode.is_dry_run(),
    });

    let mut results = Vec::with_capacity(total_items);
    let mut deleted = 0usize;
    let mut failed = 0usize;
    let mut freed_bytes = 0u64;

    for (index, item) in items.into_iter().enumerate() {
        let outcome = if mode.is_dry_run() {
            ItemOutcome::NotAttempted
        } else {
            perform_delete(platform, &item)
        };

        let freed = if outcome == ItemOutcome::Deleted {
            item.size
        } else {
            0
        };
        freed_bytes += freed;
        match outcome {
            ItemOutcome::Deleted => deleted += 1,
            ItemOutcome::Failed { .. } => failed += 1,
            ItemOutcome::NotAttempted | ItemOutcome::Missing => {}
        }

        on_progress(DeleteProgress::ItemFinished {
            path: item.path.clone(),
            outcome: outcome.clone(),
            freed,
            done: index + 1,
            total: total_items,
        });

        results.push(ItemResult {
            rule_id: item.rule_id,
            path: item.path,
            size: item.size,
            action: item.action,
            outcome,
        });
    }

    on_progress(DeleteProgress::Finished {
        deleted,
        failed,
        freed_bytes,
    });

    DeleteOutcome {
        mode,
        results,
        excluded,
    }
}

/// 1項目を実際に削除する。実行直前にパスの存在を確認し、既に無ければ
/// `Missing` として扱う（プレビューと実行の間にファイルが変化することは
/// `%TEMP%` 等では日常的に起こるため、これはエラーではない）。
fn perform_delete(platform: &dyn Platform, item: &PlannedDeletion) -> ItemOutcome {
    if fs::symlink_metadata(&item.path).is_err() {
        return ItemOutcome::Missing;
    }

    let result = match item.action {
        DeleteAction::ToTrash => platform.to_trash(&item.path).map_err(|e| e.to_string()),
        DeleteAction::RemovePath => remove_path(&item.path).map_err(|e| e.to_string()),
        DeleteAction::EmptyContainer => empty_container(&item.path).map_err(|e| e.to_string()),
    };

    match result {
        Ok(()) => ItemOutcome::Deleted,
        Err(message) => ItemOutcome::Failed { message },
    }
}

/// `path` を恒久削除する。シンボリックリンク・リパースポイントはリンク先を
/// 辿らず、リンク自体だけを消す（Windows のディレクトリシンボリックリンク・
/// ジャンクションは `remove_file` が失敗するため `remove_dir` を再試行する）。
fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        fs::remove_file(path).or_else(|_| fs::remove_dir(path))
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// `path`（コンテナ）直下の子だけを恒久削除する。コンテナ自体は残す
/// （ゴミ箱を空にする操作、#17）。一部の子が削除できなくても残りは試行し、
/// 1件でも失敗があれば全体を失敗として返す。
fn empty_container(path: &Path) -> io::Result<()> {
    let read_dir = fs::read_dir(path)?;
    let mut last_err = None;

    for child in read_dir {
        match child {
            Ok(child) => {
                if let Err(e) = remove_path(&child.path()) {
                    last_err = Some(e);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }

    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::PlatformError;
    use crate::rule::{MatchKind, Safety};
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    struct FakePlatform {
        dirs: HashMap<KnownDir, PathBuf>,
        trash_dir: PathBuf,
        trashed: RefCell<Vec<PathBuf>>,
        fail_trash_for: HashSet<PathBuf>,
        admin_paths: HashSet<PathBuf>,
    }

    impl FakePlatform {
        fn new(trash_dir: PathBuf) -> Self {
            FakePlatform {
                dirs: HashMap::new(),
                trash_dir,
                trashed: RefCell::new(Vec::new()),
                fail_trash_for: HashSet::new(),
                admin_paths: HashSet::new(),
            }
        }

        fn with_dir(mut self, kind: KnownDir, path: PathBuf) -> Self {
            self.dirs.insert(kind, path);
            self
        }

        fn fail_trash(mut self, path: PathBuf) -> Self {
            self.fail_trash_for.insert(path);
            self
        }

        fn admin_path(mut self, path: PathBuf) -> Self {
            self.admin_paths.insert(path);
            self
        }
    }

    impl Platform for FakePlatform {
        fn known_dir(&self, kind: KnownDir) -> Option<PathBuf> {
            self.dirs.get(&kind).cloned()
        }

        fn to_trash(&self, path: &Path) -> crate::platform::Result<()> {
            if self.fail_trash_for.contains(path) {
                return Err(PlatformError::Trash("simulated failure".to_string()));
            }
            // fs::rename で「消えたが復旧可能」という trash の性質を模す。
            let dest = self.trash_dir.join(path.file_name().unwrap());
            fs::rename(path, &dest).map_err(PlatformError::from)?;
            self.trashed.borrow_mut().push(path.to_path_buf());
            Ok(())
        }

        fn requires_admin(&self, path: &Path) -> bool {
            self.admin_paths.contains(path)
        }

        fn config_dir(&self) -> Option<PathBuf> {
            None
        }

        fn is_elevated(&self) -> bool {
            false
        }

        fn elevate(&self, _args: &[String]) -> crate::platform::ElevateResult {
            Err(crate::platform::ElevateError::Unsupported)
        }
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn scan_entry(rule_id: &str, path: PathBuf, size: u64, selected: bool) -> ScanEntry {
        ScanEntry {
            rule_id: rule_id.to_string(),
            path,
            size,
            file_count: 1,
            modified: None,
            age_days: None,
            recommended: selected,
            reason: String::new(),
            selected,
        }
    }

    fn test_rule(id: &str, base: KnownDir, needs_admin: bool) -> Rule {
        Rule {
            id: id.to_string(),
            label: id.to_string(),
            description: "test".to_string(),
            base,
            match_kind: MatchKind::All,
            needs_admin,
            safety: Safety::Safe,
            age_threshold_days: None,
        }
    }

    fn config_with(use_trash: bool, dry_run_default: bool) -> Config {
        Config {
            use_trash,
            dry_run_default,
            ..Config::default()
        }
    }

    // ---- DeleteMode::resolve の決定表 ----

    #[test]
    fn resolve_mode_decision_table() {
        // dry_run_default=true は常にドライラン（ConfigDefault）。
        let mode = DeleteMode::resolve(&config_with(true, true), DeleteRequest::from_config());
        assert!(mode.is_dry_run());
        assert_eq!(mode.dry_run_reason(), Some(DryRunReason::ConfigDefault));
        assert_eq!(mode.method(), DeleteMethod::Trash);

        // dry_run_default=false, use_trash=true -> 実行(Trash)。
        let mode = DeleteMode::resolve(&config_with(true, false), DeleteRequest::from_config());
        assert!(!mode.is_dry_run());
        assert_eq!(mode.method(), DeleteMethod::Trash);

        // dry_run_default=false, use_trash=false -> ドライラン(TrashDisabled)。
        let mode = DeleteMode::resolve(&config_with(false, false), DeleteRequest::from_config());
        assert!(mode.is_dry_run());
        assert_eq!(mode.dry_run_reason(), Some(DryRunReason::TrashDisabled));

        // dry_run() は Config に関わらず常にドライラン。
        let mode = DeleteMode::resolve(&config_with(true, false), DeleteRequest::dry_run());
        assert_eq!(mode.dry_run_reason(), Some(DryRunReason::ExplicitRequest));

        // execute(), use_trash=true -> 実行(Trash)。
        let mode = DeleteMode::resolve(&config_with(true, false), DeleteRequest::execute());
        assert!(!mode.is_dry_run());

        // execute(), use_trash=false -> ドライラン(TrashDisabled)。完全削除への
        // 自動昇格はしない。
        let mode = DeleteMode::resolve(&config_with(false, false), DeleteRequest::execute());
        assert!(mode.is_dry_run());
        assert_eq!(mode.dry_run_reason(), Some(DryRunReason::TrashDisabled));

        // permanent_confirmed() は常に実行(Permanent)。
        let mode = DeleteMode::resolve(
            &config_with(false, true),
            DeleteRequest::permanent_confirmed(),
        );
        assert!(!mode.is_dry_run());
        assert_eq!(mode.method(), DeleteMethod::Permanent);

        // permanent_unconfirmed() はドライランに落ちる。
        let mode = DeleteMode::resolve(
            &config_with(false, true),
            DeleteRequest::permanent_unconfirmed(),
        );
        assert!(mode.is_dry_run());
        assert_eq!(
            mode.dry_run_reason(),
            Some(DryRunReason::PermanentNotConfirmed)
        );
        assert_eq!(mode.method(), DeleteMethod::Permanent);
    }

    // ---- ドライラン ----

    #[test]
    fn dry_run_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let file = base.join("a.txt");
        write_file(&file, b"x");

        let platform =
            FakePlatform::new(dir.path().join("trash")).with_dir(KnownDir::UserTemp, base);
        let rules = vec![test_rule("user_temp", KnownDir::UserTemp, false)];
        let entries = vec![scan_entry("user_temp", file.clone(), 1, true)];

        let mode = DeleteMode::resolve(&Config::default(), DeleteRequest::dry_run());
        let plan = preview(&platform, &entries, &rules, mode);
        assert_eq!(plan.item_count(), 1);

        let outcome = execute(&platform, plan);
        assert!(outcome.is_dry_run());
        assert_eq!(outcome.deleted_count(), 0);
        assert!(
            outcome
                .results
                .iter()
                .all(|r| r.outcome == ItemOutcome::NotAttempted)
        );
        assert!(file.exists());
        assert!(platform.trashed.borrow().is_empty());
    }

    // ---- ゴミ箱送り・selected の反映 ----

    #[test]
    fn trash_mode_moves_only_selected_entries() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let trash = dir.path().join("trash");
        fs::create_dir_all(&trash).unwrap();
        let keep = base.join("keep.txt");
        let gone = base.join("gone.txt");
        write_file(&keep, b"keep");
        write_file(&gone, b"gone");

        let platform = FakePlatform::new(trash.clone()).with_dir(KnownDir::UserTemp, base.clone());
        let rules = vec![test_rule("user_temp", KnownDir::UserTemp, false)];
        let entries = vec![
            scan_entry("user_temp", keep.clone(), 4, false),
            scan_entry("user_temp", gone.clone(), 4, true),
        ];

        let mode = DeleteMode::resolve(&config_with(true, false), DeleteRequest::execute());
        let plan = preview(&platform, &entries, &rules, mode);
        assert_eq!(
            plan.item_count(),
            1,
            "F-DEL-03: selected=false は計画に含まれない"
        );
        assert_eq!(plan.total_size(), 4, "F-DEL-04: 選択分のみ再集計される");
        assert_eq!(plan.excluded().len(), 1);
        assert_eq!(plan.excluded()[0].reason, ExclusionReason::NotSelected);

        let outcome = execute(&platform, plan);
        assert_eq!(outcome.deleted_count(), 1);
        assert!(keep.exists(), "selected=false は消えない");
        assert!(!gone.exists());
        assert!(trash.join("gone.txt").exists());
    }

    // ---- 許可リスト方式の防御 ----

    #[test]
    fn unknown_rule_id_is_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let file = base.join("a.txt");
        write_file(&file, b"x");
        let platform =
            FakePlatform::new(dir.path().join("trash")).with_dir(KnownDir::UserTemp, base);
        let rules = vec![test_rule("user_temp", KnownDir::UserTemp, false)];
        let entries = vec![scan_entry("not_a_real_rule", file.clone(), 1, true)];

        let mode = DeleteMode::resolve(&Config::default(), DeleteRequest::permanent_confirmed());
        let plan = preview(&platform, &entries, &rules, mode);
        assert!(plan.is_empty());
        assert_eq!(plan.excluded()[0].reason, ExclusionReason::UnknownRule);

        let _ = execute(&platform, plan);
        assert!(file.exists());
    }

    #[test]
    fn needs_admin_rule_is_excluded_even_if_selected() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let file = base.join("a.txt");
        write_file(&file, b"x");
        let platform =
            FakePlatform::new(dir.path().join("trash")).with_dir(KnownDir::SystemTemp, base);
        let rules = vec![test_rule("system_temp", KnownDir::SystemTemp, true)];
        let entries = vec![scan_entry("system_temp", file.clone(), 1, true)];

        let mode = DeleteMode::resolve(&Config::default(), DeleteRequest::permanent_confirmed());
        let plan = preview(&platform, &entries, &rules, mode);
        assert!(plan.is_empty());
        assert_eq!(plan.excluded()[0].reason, ExclusionReason::NeedsAdmin);

        let _ = execute(&platform, plan);
        assert!(file.exists());
    }

    #[test]
    fn requires_admin_path_is_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let file = base.join("a.txt");
        write_file(&file, b"x");
        let platform = FakePlatform::new(dir.path().join("trash"))
            .with_dir(KnownDir::UserTemp, base)
            .admin_path(file.clone());
        let rules = vec![test_rule("user_temp", KnownDir::UserTemp, false)];
        let entries = vec![scan_entry("user_temp", file.clone(), 1, true)];

        let mode = DeleteMode::resolve(&Config::default(), DeleteRequest::permanent_confirmed());
        let plan = preview(&platform, &entries, &rules, mode);
        assert!(plan.is_empty());
        assert_eq!(plan.excluded()[0].reason, ExclusionReason::NeedsAdmin);
    }

    #[test]
    fn entries_outside_the_rule_base_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        fs::create_dir_all(&base).unwrap();
        let outside = dir.path().join("outside.txt");
        write_file(&outside, b"x");

        let platform =
            FakePlatform::new(dir.path().join("trash")).with_dir(KnownDir::UserTemp, base.clone());
        let rules = vec![test_rule("user_temp", KnownDir::UserTemp, false)];
        let entries = vec![
            scan_entry("user_temp", outside.clone(), 1, true),
            scan_entry("user_temp", base.clone(), 0, true),
        ];

        let mode = DeleteMode::resolve(&Config::default(), DeleteRequest::permanent_confirmed());
        let plan = preview(&platform, &entries, &rules, mode);
        assert!(plan.is_empty());
        assert!(
            plan.excluded()
                .iter()
                .all(|e| e.reason == ExclusionReason::OutsideRuleBase)
        );
        assert!(outside.exists());
    }

    // ---- 完全削除 ----

    #[test]
    fn permanent_mode_removes_files_directly_without_using_to_trash() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let file = base.join("a.txt");
        write_file(&file, b"x");
        let platform =
            FakePlatform::new(dir.path().join("trash")).with_dir(KnownDir::Downloads, base);
        let rules = vec![test_rule("old_downloads", KnownDir::Downloads, false)];
        let entries = vec![scan_entry("old_downloads", file.clone(), 1, true)];

        let mode = DeleteMode::resolve(&Config::default(), DeleteRequest::permanent_confirmed());
        let plan = preview(&platform, &entries, &rules, mode);
        assert_eq!(plan.items()[0].action, DeleteAction::RemovePath);

        let outcome = execute(&platform, plan);
        assert_eq!(outcome.deleted_count(), 1);
        assert!(!file.exists());
        assert!(
            platform.trashed.borrow().is_empty(),
            "完全削除では to_trash を使わない"
        );
    }

    // ---- recycle_bin（Issue #17）----

    #[test]
    fn recycle_bin_entries_require_permanent_mode_and_keep_the_container() {
        let dir = tempfile::tempdir().unwrap();
        let recycle_root = dir.path().join("Recycle");
        let sid_dir = recycle_root.join("S-1-5-21-example");
        write_file(&sid_dir.join("deleted.txt"), b"x");

        let platform = FakePlatform::new(dir.path().join("trash"))
            .with_dir(KnownDir::RecycleBin, recycle_root);
        let rules = vec![test_rule("recycle_bin", KnownDir::RecycleBin, false)];
        let entries = vec![scan_entry("recycle_bin", sid_dir.clone(), 1, true)];

        // 通常のゴミ箱送りモードでは実行しない（#17）。
        let mode = DeleteMode::resolve(&config_with(true, false), DeleteRequest::execute());
        let plan = preview(&platform, &entries, &rules, mode);
        assert!(plan.is_empty());
        assert_eq!(
            plan.excluded()[0].reason,
            ExclusionReason::RequiresPermanent
        );
        assert!(sid_dir.join("deleted.txt").exists());

        // 完全削除モードでは中身だけ消え、コンテナ自体は残る。
        let mode = DeleteMode::resolve(&Config::default(), DeleteRequest::permanent_confirmed());
        let plan = preview(&platform, &entries, &rules, mode);
        assert_eq!(plan.items()[0].action, DeleteAction::EmptyContainer);

        let outcome = execute(&platform, plan);
        assert_eq!(outcome.deleted_count(), 1);
        assert!(sid_dir.exists(), "コンテナ自体は残る");
        assert!(!sid_dir.join("deleted.txt").exists());
    }

    // ---- エラー集約 ----

    #[test]
    fn one_failure_does_not_stop_other_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let ok_file = base.join("ok.txt");
        let bad_file = base.join("bad.txt");
        write_file(&ok_file, b"x");
        write_file(&bad_file, b"x");

        let trash = dir.path().join("trash");
        fs::create_dir_all(&trash).unwrap();
        let platform = FakePlatform::new(trash)
            .with_dir(KnownDir::UserTemp, base)
            .fail_trash(bad_file.clone());
        let rules = vec![test_rule("user_temp", KnownDir::UserTemp, false)];
        let entries = vec![
            scan_entry("user_temp", ok_file.clone(), 1, true),
            scan_entry("user_temp", bad_file.clone(), 1, true),
        ];

        let mode = DeleteMode::resolve(&config_with(true, false), DeleteRequest::execute());
        let plan = preview(&platform, &entries, &rules, mode);
        let outcome = execute(&platform, plan);

        assert_eq!(outcome.deleted_count(), 1);
        assert_eq!(outcome.failed_count(), 1);
        assert_eq!(outcome.freed_bytes(), 1, "失敗分はfreed_bytesに含めない");
        assert!(!ok_file.exists());
        assert!(bad_file.exists());
    }

    #[test]
    fn missing_file_at_execution_time_is_reported_as_missing_not_failed() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let file = base.join("a.txt");
        write_file(&file, b"x");
        let platform =
            FakePlatform::new(dir.path().join("trash")).with_dir(KnownDir::UserTemp, base);
        let rules = vec![test_rule("user_temp", KnownDir::UserTemp, false)];
        let entries = vec![scan_entry("user_temp", file.clone(), 1, true)];

        let mode = DeleteMode::resolve(&config_with(true, false), DeleteRequest::execute());
        let plan = preview(&platform, &entries, &rules, mode);

        fs::remove_file(&file).unwrap(); // プレビュー後に消える

        let outcome = execute(&platform, plan);
        assert_eq!(outcome.results[0].outcome, ItemOutcome::Missing);
        assert_eq!(outcome.failed_count(), 0);
    }
}
