//! 削除実行の履歴・実績の記録（F-HIST-01〜05 / C4 / Issue #50）。
//!
//! `config.rs` と同じ「`Platform::config_dir()` 配下に JSON を1つ置く」方式
//! を踏襲するが、[`config::load`] とは異なり、壊れたファイルを黙って既定値
//! へフォールバックさせない（[`load`] を参照）。設定は失っても実行し直せば
//! 復元できるが、実績は再現できないため、静かに消してよいものではない。
//!
//! パス（削除対象の実パス）は記録しない。履歴ファイルが機微情報
//! （どのファイルをいつ削除したか）を保持しないようにするため（NF-SAF-04
//! の許可リスト方式と同様、必要最小限の情報のみを永続化する設計）。

use crate::delete::{DeleteMethod, DeleteOutcome};
use crate::platform::Platform;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 1回の削除実行の記録。パスは記録しない（履歴ファイルを機微情報化しない
/// ため）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// 実行時刻（UNIX epoch 秒）。`SystemTime` をそのままシリアライズすると
    /// 表現がプラットフォーム依存になり得るため、秒数に変換して保持する。
    pub timestamp_secs: u64,
    /// 解放されたバイト数（成功分のみ）。
    pub freed_bytes: u64,
    /// 削除に成功した件数。
    pub deleted_count: usize,
    /// 削除に失敗した件数。
    pub failed_count: usize,
    /// 完全削除（ゴミ箱を経由しない）だったか。
    pub permanent: bool,
}

/// 削除実行履歴の集合。
///
/// `#[serde(default)]`（コンテナ属性）により、将来フィールドを追加しても
/// 既存の `history.json` の読み込みが壊れない（`config.rs` と同じ考え方）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    /// 実行順（古い順）に並んだ記録。
    pub entries: Vec<HistoryEntry>,
}

/// 保持する記録の上限件数。これを超えた分は古いものから間引く。
pub const MAX_ENTRIES: usize = 100;

impl History {
    /// これまでに解放した合計バイト数。
    pub fn total_freed_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.freed_bytes).sum()
    }

    /// 記録されている実行回数。
    pub fn run_count(&self) -> usize {
        self.entries.len()
    }

    /// 新しい順に、最大 `limit` 件を返す。
    pub fn recent(&self, limit: usize) -> Vec<&HistoryEntry> {
        self.entries.iter().rev().take(limit).collect()
    }
}

/// `outcome` から履歴記録を作る。ドライラン、または削除0件（全滅失敗・
/// 対象なし含む）の場合は記録する意味がないため `None` を返す。
pub fn entry_from_outcome(outcome: &DeleteOutcome, now: SystemTime) -> Option<HistoryEntry> {
    if outcome.is_dry_run() {
        return None;
    }
    let deleted_count = outcome.deleted_count();
    if deleted_count == 0 {
        return None;
    }

    let timestamp_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Some(HistoryEntry {
        timestamp_secs,
        freed_bytes: outcome.freed_bytes(),
        deleted_count,
        failed_count: outcome.failed_count(),
        permanent: outcome.method() == DeleteMethod::Permanent,
    })
}

/// `history` へ `entry` を追記し、[`MAX_ENTRIES`] を超えた分を古い順に
/// 間引く。
pub fn push(history: &mut History, entry: HistoryEntry) {
    history.entries.push(entry);
    if history.entries.len() > MAX_ENTRIES {
        let excess = history.entries.len() - MAX_ENTRIES;
        history.entries.drain(0..excess);
    }
}

/// 履歴ファイル名。
const HISTORY_FILE_NAME: &str = "history.json";

/// `Platform::config_dir()` を用いて履歴ファイルのフルパス
/// （`<config_dir>/history.json`）を解決する。`config_dir` が解決できない
/// 場合は `None`。`config.rs` の `config_file_path` と同じ考え方で、実際の
/// ファイル I/O はここでは行わない。
pub fn history_file_path(platform: &dyn Platform) -> Option<PathBuf> {
    platform.config_dir().map(|dir| dir.join(HISTORY_FILE_NAME))
}

/// 履歴ファイルを読み込む。
///
/// `config::load()` とは異なり、ファイルが存在しない場合と壊れている場合を
/// 区別する： 不在は「まだ実績がない」ことを意味するため空の履歴として
/// 扱うが、壊れたファイル（パース失敗）は `Err` を返す。実績は設定と違い
/// 消えても実行し直せば復元できるものではないため、黙って握りつぶして
/// 上書き保存（実質消失）してしまうことを避ける。
pub fn load(path: &Path) -> std::io::Result<History> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(History::default()),
        Err(e) => Err(e),
    }
}

/// 履歴を `path` に保存する。親ディレクトリが存在しなければ作成する。
pub fn save(history: &History, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(history)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

/// 削除実行の結果を履歴へ記録する（読み込み → 追記 → 剪定 → 保存）。
///
/// 保存先（`Platform::config_dir()`）が特定できない場合は `None` を返す。
/// `outcome` がドライラン、または削除0件の場合は既存の履歴を読み込んで
/// そのまま返す（記録は追加しないが、呼び出し側が最新の `History` を
/// 保持し直せるようにする）。
pub fn record_outcome(
    platform: &dyn Platform,
    outcome: &DeleteOutcome,
) -> Option<std::io::Result<History>> {
    let path = history_file_path(platform)?;
    Some((|| {
        let mut history = load(&path)?;
        if let Some(entry) = entry_from_outcome(outcome, SystemTime::now()) {
            push(&mut history, entry);
            save(&history, &path)?;
        }
        Ok(history)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::delete::{DeleteMode, DeleteRequest, execute};
    use crate::entry::ScanEntry;
    use crate::platform::{ElevateError, ElevateResult, KnownDir, Platform, PlatformError};
    use crate::rule::{MatchKind, Rule, Safety};
    use std::path::Path;

    /// `delete.rs` のテスト用 `FakePlatform` と同じ考え方の最小実装。
    /// `entry_from_outcome` は `DeleteOutcome` の私有フィールドに依存するため、
    /// 実際に `preview()` / `execute()` を通して `DeleteOutcome` を作る必要が
    /// あり、そのために最低限の `Platform` 実装が要る。
    struct FakePlatform {
        temp_dir: PathBuf,
        trash_dir: PathBuf,
    }

    impl FakePlatform {
        fn new(temp_dir: PathBuf) -> Self {
            let trash_dir = temp_dir.join("__trash__");
            std::fs::create_dir_all(&trash_dir).unwrap();
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
            std::fs::rename(path, dest).map_err(PlatformError::from)
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

    fn sample_entry() -> HistoryEntry {
        HistoryEntry {
            timestamp_secs: 1_700_000_000,
            freed_bytes: 1024,
            deleted_count: 3,
            failed_count: 1,
            permanent: false,
        }
    }

    #[test]
    fn total_freed_bytes_sums_all_entries() {
        let mut history = History::default();
        push(&mut history, sample_entry());
        push(
            &mut history,
            HistoryEntry {
                freed_bytes: 2048,
                ..sample_entry()
            },
        );
        assert_eq!(history.total_freed_bytes(), 1024 + 2048);
        assert_eq!(history.run_count(), 2);
    }

    #[test]
    fn push_caps_at_max_entries_and_keeps_the_newest() {
        let mut history = History::default();
        for i in 0..(MAX_ENTRIES + 5) {
            push(
                &mut history,
                HistoryEntry {
                    timestamp_secs: i as u64,
                    ..sample_entry()
                },
            );
        }
        assert_eq!(history.entries.len(), MAX_ENTRIES);
        // 最も古い5件（timestamp_secs 0..5）が落ちて、直近が残っていること。
        assert_eq!(history.entries.first().unwrap().timestamp_secs, 5);
        assert_eq!(
            history.entries.last().unwrap().timestamp_secs,
            (MAX_ENTRIES + 4) as u64
        );
    }

    #[test]
    fn recent_returns_newest_first_and_respects_limit() {
        let mut history = History::default();
        for i in 0..5u64 {
            push(
                &mut history,
                HistoryEntry {
                    timestamp_secs: i,
                    ..sample_entry()
                },
            );
        }
        let recent = history.recent(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].timestamp_secs, 4);
        assert_eq!(recent[1].timestamp_secs, 3);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("history.json");

        let mut history = History::default();
        push(&mut history, sample_entry());

        save(&history, &path).unwrap();
        assert_eq!(load(&path).unwrap(), history);
    }

    #[test]
    fn load_returns_empty_history_when_file_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert_eq!(load(&path).unwrap(), History::default());
    }

    #[test]
    fn load_returns_error_when_file_is_corrupt() {
        // `config::load` はファイル破損時に既定値へ黙ってフォールバックする
        // （設定は失っても実行し直せば再構築できるため）。しかし履歴は
        // 過去の実績そのものであり、再現不可能なため、破損時に黙って
        // 空へ差し替えてはならない。ここでは意図的に `config::load` と
        // 挙動を分岐させている。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(&path, "not valid json").unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn load_ignores_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(
            &path,
            r#"{"entries": [], "future_field": "some-value-from-a-newer-version"}"#,
        )
        .unwrap();
        assert_eq!(load(&path).unwrap(), History::default());
    }

    fn scannable_rule() -> Rule {
        Rule {
            id: "user_temp".to_string(),
            label: "一時ファイル".to_string(),
            description: "テスト用".to_string(),
            base: crate::platform::KnownDir::UserTemp,
            match_kind: MatchKind::All,
            needs_admin: false,
            safety: Safety::Safe,
            age_threshold_days: None,
            large_file_threshold_bytes: None,
        }
    }

    fn entry_for(path: std::path::PathBuf, size: u64) -> ScanEntry {
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

    #[test]
    fn entry_from_outcome_ignores_dry_runs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.tmp");
        std::fs::write(&file, b"hello").unwrap();

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

        assert_eq!(
            entry_from_outcome(&outcome, SystemTime::now()),
            None,
            "ドライランは記録しない"
        );
    }

    #[test]
    fn entry_from_outcome_ignores_runs_that_deleted_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let platform = FakePlatform::new(dir.path().to_path_buf());
        let config = Config {
            dry_run_default: false,
            ..Config::default()
        };
        let mode = DeleteMode::resolve(&config, DeleteRequest::from_config());
        // 対象エントリなし → 削除0件。
        let plan = crate::delete::preview(&platform, &[], &[scannable_rule()], mode);
        let outcome = execute(&platform, plan);
        assert_eq!(outcome.deleted_count(), 0);

        assert_eq!(entry_from_outcome(&outcome, SystemTime::now()), None);
    }

    #[test]
    fn entry_from_outcome_records_freed_bytes_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.tmp");
        std::fs::write(&file, b"hello").unwrap();

        let platform = FakePlatform::new(dir.path().to_path_buf());
        let config = Config {
            dry_run_default: false,
            use_trash: true,
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
        assert!(!outcome.is_dry_run());
        assert_eq!(outcome.deleted_count(), 1);

        let recorded = entry_from_outcome(
            &outcome,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(42),
        )
        .expect("削除が発生したので記録されるはず");
        assert_eq!(recorded.timestamp_secs, 42);
        assert_eq!(recorded.freed_bytes, outcome.freed_bytes());
        assert_eq!(recorded.deleted_count, 1);
        assert_eq!(recorded.failed_count, 0);
        assert!(!recorded.permanent);
    }
}
