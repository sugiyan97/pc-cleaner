//! 削除実行の監査ログ（誤削除時の追跡記録、D1 / Issue #51）。
//!
//! [`crate::history`] は「実行回数・解放バイト数・成功/失敗件数」のみを
//! 記録し、**削除対象の実パスを意図的に記録しない**（F-HIST-02、機微情報化
//! を避けるため）。しかし「いつ何を削除したか」を追跡する（誤削除発生時に
//! 何が起きたか調べる）にはパスが不可欠であり、`history.rs` の設計方針とは
//! 正面から矛盾する。そのため本モジュールは `history.rs` を拡張せず、
//! `<config_dir>/deletion_log.jsonl` という別ファイル・別ポリシーとして
//! 新設する。
//!
//! `history.rs` との違い：
//! - 記録内容：`history.rs` は集計のみ。本モジュールはパスを含む項目単位の
//!   記録（誤削除の追跡が目的のため）。
//! - 保持ポリシー：`history.rs` は件数上限（[`crate::history::MAX_ENTRIES`]）。
//!   本モジュールは容量上限（[`MAX_LOG_BYTES`]）でローテーションする
//!   （1回の削除で数千件の項目が出うるため、件数ベースは不適切）。
//! - 破損時の扱い：`history.rs` はファイル全体のパース失敗を `Err` として
//!   扱い、黙った上書きを避ける。本モジュールは JSON Lines（1行1レコード）
//!   形式を採り、**パース不能な行だけを読み飛ばして残りは読む**
//!   （[`load`] 参照）。追跡記録は1行でも多く残る方が調査に資するため。
//! - 有効/無効：`history.rs` はオプトアウト不可。本モジュールはパスという
//!   機微情報を扱うため [`crate::config::Config::audit_log_enabled`]
//!   （既定 `true`）でユーザーが無効化できる。
//!
//! 記録は [`crate::delete::execute`] / [`crate::delete::execute_with_progress`]
//! の結果（[`crate::delete::DeleteOutcome`]）から作る。監査ログの書き込みは
//! 削除の成否そのものには一切影響しない読み取り専用の副作用であり、許可
//! リスト方式（NF-SAF-04）や二次防御（`Rule::is_permitted` 等）を経由済みの
//! 結果を記録するだけなので、安全性ゲートとは独立している。

use crate::config::Config;
use crate::delete::{DeleteAction, DeleteMethod, DeleteOutcome, ItemOutcome};
use crate::platform::Platform;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// 監査ログの1件（削除が計画された項目1つに対応）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// この記録の元になった実行単位の識別子（[`next_run_id`] が発行する）。
    /// 同一の `execute()` 呼び出しに由来するレコードは同じ値を持つ。
    pub run_id: u64,
    /// 実行時刻（UNIX epoch 秒）。
    pub timestamp_secs: u64,
    /// 由来ルールの `id`。
    pub rule_id: String,
    /// 削除対象のパス（本モジュールが `history.rs` と異なりこれを記録する
    /// 理由はモジュール doc を参照）。
    pub path: PathBuf,
    /// 計画時点のサイズ（バイト）。
    pub size: u64,
    /// 実行された操作。
    pub action: DeleteAction,
    /// 結果。
    pub outcome: ItemOutcome,
    /// この実行全体の削除方法（ゴミ箱経由か完全削除か）。
    pub method: DeleteMethod,
}

/// 保持する監査ログの上限バイト数。超えた場合は1世代だけローテーションする
/// （[`append`] 参照）。
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// 監査ログファイル名。JSON Lines（1行1レコード）形式。
const AUDIT_FILE_NAME: &str = "deletion_log.jsonl";
/// ローテーション先のバックアップファイル名。1世代のみ保持し、既存の
/// バックアップは新しいもので上書きする。
const AUDIT_BACKUP_FILE_NAME: &str = "deletion_log.1.jsonl";

/// 実行単位の識別子を発行する。プロセス内で単調増加することを保証する
/// （タイムスタンプの秒精度だけでは同一秒内の複数回実行が衝突しうるため、
/// プロセス内カウンタを下位ビットに埋め込む）。[`crate::history::HistoryEntry`]
/// / [`AuditRecord`] の双方でこの値を共有し、ロールバック補助（D3 /
/// Issue #53）が「この実行をまとめて元に戻す」導線に使う想定。
pub fn next_run_id(now: SystemTime) -> u64 {
    static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

    let timestamp_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let counter = RUN_COUNTER.fetch_add(1, Ordering::Relaxed) % 1_000_000;
    timestamp_secs * 1_000_000 + counter
}

/// `outcome` から監査レコードを作る純粋関数（I/O なし）。ドライランの場合は
/// 何も起きていないため空の `Vec` を返す（ドライラン結果の出力はエクスポート
/// 機能・Issue #52 が別途担う）。
///
/// `results` の全件（`Deleted` / `Failed` / `Missing`）を記録する。「消えた
/// もの」だけでなく「消せなかったもの」も誤削除調査には価値があり、
/// ロールバック補助（#53）が `action == ToTrash && outcome == Deleted` で
/// 絞り込めるようにするため、`DeleteOutcome::results` の忠実な写しとする。
pub fn records_from_outcome(
    outcome: &DeleteOutcome,
    run_id: u64,
    now: SystemTime,
) -> Vec<AuditRecord> {
    if outcome.is_dry_run() {
        return Vec::new();
    }

    let timestamp_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let method = outcome.method();

    outcome
        .results
        .iter()
        .map(|item| AuditRecord {
            run_id,
            timestamp_secs,
            rule_id: item.rule_id.clone(),
            path: item.path.clone(),
            size: item.size,
            action: item.action,
            outcome: item.outcome.clone(),
            method,
        })
        .collect()
}

/// `records` のうち `run_id` に一致するものだけを返す（ロールバック補助・
/// Issue #53 が「この実行分だけ元に戻す」ために使う想定の契約）。
pub fn filter_by_run(records: &[AuditRecord], run_id: u64) -> Vec<&AuditRecord> {
    records.iter().filter(|r| r.run_id == run_id).collect()
}

/// `Platform::config_dir()` を用いて監査ログファイルのフルパス
/// （`<config_dir>/deletion_log.jsonl`）を解決する。`config_dir` が解決
/// できない場合は `None`。
pub fn audit_file_path(platform: &dyn Platform) -> Option<PathBuf> {
    platform.config_dir().map(|dir| dir.join(AUDIT_FILE_NAME))
}

fn backup_file_path(path: &Path) -> PathBuf {
    path.with_file_name(AUDIT_BACKUP_FILE_NAME)
}

/// `path` のサイズが [`MAX_LOG_BYTES`] を超えていれば、既存の内容を
/// バックアップ（`deletion_log.1.jsonl`）へ丸ごと退避する（1世代のみ保持。
/// 既存のバックアップがあれば上書きされる）。追記前に呼ぶことで、
/// ファイルが無限に肥大化しないようにする。
fn rotate_if_needed(path: &Path) -> io::Result<()> {
    let len = match fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if len < MAX_LOG_BYTES {
        return Ok(());
    }
    fs::rename(path, backup_file_path(path))
}

/// `records` を `path` へ追記する（JSON Lines、1レコード1行）。親ディレクトリ
/// が存在しなければ作成する。空の `records` は何もせず `Ok(0)` を返す。
///
/// `history.rs` の load-modify-save とは異なり、既存の内容を読み直さずに
/// 追記のみ行う（1回の `OpenOptions::append` で完結し、競合ウィンドウが
/// 無い）。戻り値は書き込んだ件数。
pub fn append(records: &[AuditRecord], path: &Path) -> io::Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    rotate_if_needed(path)?;

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    for record in records {
        let line = serde_json::to_string(record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{line}")?;
    }
    Ok(records.len())
}

/// 監査ログを読み込む。`history::load` とは異なり、行単位でパースを試み、
/// **パース不能な行はスキップして残りの行は読む**（モジュール doc 参照）。
/// ファイルが存在しない場合は空の `Vec`。
pub fn load(path: &Path) -> io::Result<Vec<AuditRecord>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    Ok(contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<AuditRecord>(line).ok())
        .collect())
}

/// [`crate::history::record_outcome`] と対になる高レベル API。
/// `config.audit_log_enabled` が `false` の場合は何も書かず `Some(Ok(0))` を
/// 返す（呼び出し側が「無効化されているだけ」と「保存先を特定できない」を
/// 区別できるようにする）。保存先（`Platform::config_dir()`）が特定できない
/// 場合は `None`。
pub fn record_outcome(
    platform: &dyn Platform,
    config: &Config,
    outcome: &DeleteOutcome,
    run_id: u64,
) -> Option<io::Result<usize>> {
    if !config.audit_log_enabled {
        return Some(Ok(0));
    }
    let path = audit_file_path(platform)?;
    let records = records_from_outcome(outcome, run_id, SystemTime::now());
    Some(append(&records, &path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delete::{DeleteMode, DeleteRequest, execute};
    use crate::entry::ScanEntry;
    use crate::platform::{ElevateError, ElevateResult, KnownDir, PlatformError};
    use crate::rule::{MatchKind, Rule, Safety};
    use std::path::Path;

    /// `history.rs` の `FakePlatform` と同じ考え方の最小実装。
    struct FakePlatform {
        temp_dir: PathBuf,
        trash_dir: PathBuf,
    }

    impl FakePlatform {
        fn new(temp_dir: PathBuf) -> Self {
            let trash_dir = temp_dir.join("__trash__");
            fs::create_dir_all(&trash_dir).unwrap();
            FakePlatform {
                temp_dir,
                trash_dir,
            }
        }
    }

    impl Platform for FakePlatform {
        fn known_dir(&self, kind: KnownDir) -> Option<PathBuf> {
            match kind {
                KnownDir::UserTemp => Some(self.temp_dir.clone()),
                _ => None,
            }
        }

        fn to_trash(&self, path: &Path) -> crate::platform::Result<()> {
            let dest = self.trash_dir.join(path.file_name().unwrap());
            fs::rename(path, dest).map_err(PlatformError::from)
        }

        fn requires_admin(&self, _path: &Path) -> bool {
            false
        }

        fn config_dir(&self) -> Option<PathBuf> {
            None
        }

        fn is_elevated(&self) -> bool {
            false
        }

        fn elevate(&self, _args: &[String]) -> ElevateResult {
            Err(ElevateError::Unsupported)
        }
    }

    fn scannable_rule() -> Rule {
        Rule {
            id: "user_temp".to_string(),
            label: "一時ファイル".to_string(),
            description: "テスト用".to_string(),
            base: KnownDir::UserTemp,
            match_kind: MatchKind::All,
            needs_admin: false,
            safety: Safety::Safe,
            age_threshold_days: None,
            large_file_threshold_bytes: None,
        }
    }

    fn entry_for(path: PathBuf, size: u64) -> ScanEntry {
        ScanEntry {
            rule_id: "user_temp".to_string(),
            path,
            size,
            file_count: 1,
            modified: None,
            age_days: None,
            in_use: None,
            duplicate: None,
            recommended: true,
            reason: String::new(),
            selected: true,
        }
    }

    fn sample_record(run_id: u64, path: &str) -> AuditRecord {
        AuditRecord {
            run_id,
            timestamp_secs: 1_700_000_000,
            rule_id: "user_temp".to_string(),
            path: PathBuf::from(path),
            size: 1024,
            action: DeleteAction::ToTrash,
            outcome: ItemOutcome::Deleted,
            method: DeleteMethod::Trash,
        }
    }

    // ---- records_from_outcome ----

    #[test]
    fn records_from_outcome_ignores_dry_runs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.tmp");
        fs::write(&file, b"hello").unwrap();

        let platform = FakePlatform::new(dir.path().to_path_buf());
        let config = Config::default(); // dry_run_default = true
        let mode = DeleteMode::resolve(&config, DeleteRequest::from_config());
        let plan = crate::delete::preview(
            &platform,
            std::slice::from_ref(&entry_for(file, 5)),
            std::slice::from_ref(&scannable_rule()),
            mode,
        );
        let outcome = execute(&platform, plan);
        assert!(outcome.is_dry_run());

        assert!(records_from_outcome(&outcome, 1, SystemTime::now()).is_empty());
    }

    #[test]
    fn records_from_outcome_includes_all_results_not_just_successes() {
        let dir = tempfile::tempdir().unwrap();
        let ok_file = dir.path().join("ok.tmp");
        let missing_file = dir.path().join("missing.tmp");
        fs::write(&ok_file, b"hello").unwrap();

        let platform = FakePlatform::new(dir.path().to_path_buf());
        let config = Config {
            dry_run_default: false,
            use_trash: true,
            ..Config::default()
        };
        let mode = DeleteMode::resolve(&config, DeleteRequest::from_config());
        let entries = vec![
            entry_for(ok_file.clone(), 5),
            entry_for(missing_file.clone(), 3),
        ];
        let plan = crate::delete::preview(&platform, &entries, &[scannable_rule()], mode);
        let outcome = execute(&platform, plan);

        let records = records_from_outcome(&outcome, 42, SystemTime::UNIX_EPOCH);
        assert_eq!(records.len(), 2, "成功も Missing もどちらも記録する");
        assert!(records.iter().all(|r| r.run_id == 42));
        let ok_record = records.iter().find(|r| r.path == ok_file).unwrap();
        assert_eq!(ok_record.outcome, ItemOutcome::Deleted);
        assert_eq!(ok_record.action, DeleteAction::ToTrash);
        assert_eq!(ok_record.method, DeleteMethod::Trash);
        let missing_record = records.iter().find(|r| r.path == missing_file).unwrap();
        assert_eq!(missing_record.outcome, ItemOutcome::Missing);
    }

    // ---- next_run_id ----

    #[test]
    fn next_run_id_is_unique_and_increasing_within_a_process() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let a = next_run_id(now);
        let b = next_run_id(now);
        assert_ne!(a, b);
        assert!(b > a);
    }

    // ---- append / load ----

    #[test]
    fn append_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deletion_log.jsonl");

        let records = vec![sample_record(1, "a.tmp"), sample_record(1, "b.tmp")];
        let written = append(&records, &path).unwrap();
        assert_eq!(written, 2);
        assert_eq!(load(&path).unwrap(), records);
    }

    #[test]
    fn append_with_empty_records_does_not_create_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deletion_log.jsonl");
        assert_eq!(append(&[], &path).unwrap(), 0);
        assert!(!path.exists());
    }

    #[test]
    fn load_returns_empty_vec_when_file_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        assert_eq!(load(&path).unwrap(), Vec::new());
    }

    #[test]
    fn load_skips_corrupt_lines_but_keeps_the_rest() {
        // history::load はファイル全体の破損を Err にするが、audit::load は
        // 行単位のスキップに倒す（モジュール doc の対比を参照）。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deletion_log.jsonl");
        let good = serde_json::to_string(&sample_record(1, "a.tmp")).unwrap();
        fs::write(&path, format!("{good}\nnot valid json\n\n{good}\n")).unwrap();

        let records = load(&path).unwrap();
        assert_eq!(records.len(), 2, "壊れた行はスキップし、残りは読める");
    }

    #[test]
    fn append_rotates_to_backup_when_exceeding_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deletion_log.jsonl");
        // MAX_LOG_BYTES を明示的に超える内容を先に書いておく。
        fs::write(&path, "x".repeat(MAX_LOG_BYTES as usize + 1)).unwrap();

        append(&[sample_record(1, "a.tmp")], &path).unwrap();

        let backup = backup_file_path(&path);
        assert!(backup.exists(), "旧内容はバックアップへ退避される");
        let current = load(&path).unwrap();
        assert_eq!(current.len(), 1, "新しい内容だけが現行ファイルに残る");
    }

    // ---- filter_by_run ----

    #[test]
    fn filter_by_run_only_returns_matching_records() {
        let records = vec![
            sample_record(1, "a.tmp"),
            sample_record(2, "b.tmp"),
            sample_record(1, "c.tmp"),
        ];
        let filtered = filter_by_run(&records, 1);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|r| r.run_id == 1));
    }

    // ---- record_outcome ----

    #[test]
    fn record_outcome_does_nothing_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.tmp");
        fs::write(&file, b"hello").unwrap();

        struct PlatformWithConfigDir {
            inner: FakePlatform,
            config_dir: PathBuf,
        }
        impl Platform for PlatformWithConfigDir {
            fn known_dir(&self, kind: KnownDir) -> Option<PathBuf> {
                self.inner.known_dir(kind)
            }
            fn to_trash(&self, path: &Path) -> crate::platform::Result<()> {
                self.inner.to_trash(path)
            }
            fn requires_admin(&self, path: &Path) -> bool {
                self.inner.requires_admin(path)
            }
            fn config_dir(&self) -> Option<PathBuf> {
                Some(self.config_dir.clone())
            }
            fn is_elevated(&self) -> bool {
                self.inner.is_elevated()
            }
            fn elevate(&self, args: &[String]) -> ElevateResult {
                self.inner.elevate(args)
            }
        }

        let config_dir = dir.path().join("config");
        let platform = PlatformWithConfigDir {
            inner: FakePlatform::new(dir.path().to_path_buf()),
            config_dir: config_dir.clone(),
        };
        let config = Config {
            dry_run_default: false,
            use_trash: true,
            audit_log_enabled: false,
            ..Config::default()
        };
        let mode = DeleteMode::resolve(&config, DeleteRequest::from_config());
        let plan = crate::delete::preview(
            &platform,
            std::slice::from_ref(&entry_for(file, 5)),
            std::slice::from_ref(&scannable_rule()),
            mode,
        );
        let outcome = execute(&platform, plan);

        let result = record_outcome(&platform, &config, &outcome, 1);
        assert_eq!(result.unwrap().unwrap(), 0);
        assert!(!audit_file_path(&platform).unwrap().exists());
    }

    #[test]
    fn record_outcome_returns_none_when_config_dir_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        let platform = FakePlatform::new(dir.path().to_path_buf()); // config_dir() == None
        let config = Config::default();
        let plan = crate::delete::preview(
            &platform,
            &[],
            &[scannable_rule()],
            DeleteMode::resolve(&config, DeleteRequest::dry_run()),
        );
        let outcome = execute(&platform, plan);
        assert!(record_outcome(&platform, &config, &outcome, 1).is_none());
    }
}
