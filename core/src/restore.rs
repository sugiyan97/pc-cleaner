//! ロールバック補助（ゴミ箱からの復元、D3 / Issue #53）。
//!
//! ゴミ箱の一覧（[`Platform::list_trash`]）と監査ログ（[`crate::audit`] /
//! D1・Issue #51）を突き合わせ、「pc-cleaner がいつのどの実行で削除したか」
//! が分かる項目を識別する（[`correlate`]）。監査ログと突き合わない項目
//! （他アプリが削除したもの、または監査ログが無効化されていた期間に
//! 削除されたもの）も一覧には含めるが、由来が分からないことを明示する
//! （`rule_id` / `run_id` が `None`）。
//!
//! 復元は削除ではないため許可リスト方式（NF-SAF-04）の対象外だが、
//! **復元によって既存ファイルを上書きしない**ことは NF-SAF-01 の観点で
//! 引き続き守る（[`crate::platform::Platform::restore_from_trash`] の
//! ドキュメント参照）。本モジュールは [`crate::delete::DeletePlan`] /
//! [`crate::delete::execute`] に一切触れない：復元経路から削除が起きる
//! ことはない。

use crate::audit::AuditRecord;
use crate::delete::{DeleteAction, ItemOutcome};
use crate::platform::{Platform, RestoreItemOutcome, TrashEntry};
use std::collections::HashMap;
use std::path::PathBuf;

/// ゴミ箱の1項目に、監査ログから分かる由来情報を付与したもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreCandidate {
    /// ゴミ箱側の情報。
    pub entry: TrashEntry,
    /// 由来ルールの `id`。監査ログと突き合わせられなかった場合は `None`
    /// （pc-cleaner が削除したものと確認できない）。
    pub rule_id: Option<String>,
    /// 由来の実行単位識別子（`run_id`）。`None` の意味は `rule_id` と同じ。
    pub run_id: Option<u64>,
}

/// `trash`（ゴミ箱の一覧）と `audit`（監査ログ）を突き合わせる純粋関数
/// （I/O なし・時刻非依存）。
///
/// キーは「`TrashEntry::original_path` と一致し、かつ
/// `action == DeleteAction::ToTrash` かつ `outcome == ItemOutcome::Deleted`
/// である監査レコード」。同一パスが複数回削除されている場合は、ゴミ箱側の
/// 削除時刻（`deleted_at_secs`）に最も近い監査レコードを採用する。
/// `deleted_at_secs` が取得できない場合は、監査ログ側で最も新しいものを
/// 採用する（両者とも決められない＝候補が1件もない場合のみ `None`）。
pub fn correlate(trash: &[TrashEntry], audit: &[AuditRecord]) -> Vec<RestoreCandidate> {
    trash
        .iter()
        .map(|entry| {
            let candidates: Vec<&AuditRecord> = audit
                .iter()
                .filter(|r| {
                    r.path == entry.original_path
                        && r.action == DeleteAction::ToTrash
                        && r.outcome == ItemOutcome::Deleted
                })
                .collect();

            let best = match entry.deleted_at_secs {
                Some(deleted_at) => candidates
                    .into_iter()
                    .min_by_key(|r| r.timestamp_secs.abs_diff(deleted_at)),
                None => candidates.into_iter().max_by_key(|r| r.timestamp_secs),
            };

            match best {
                Some(record) => RestoreCandidate {
                    entry: entry.clone(),
                    rule_id: Some(record.rule_id.clone()),
                    run_id: Some(record.run_id),
                },
                None => RestoreCandidate {
                    entry: entry.clone(),
                    rule_id: None,
                    run_id: None,
                },
            }
        })
        .collect()
}

/// `candidates` のうち `run_id` に一致するものだけを返す（「この実行分だけ
/// まとめて元に戻す」導線に使う）。
pub fn candidates_for_run(candidates: &[RestoreCandidate], run_id: u64) -> Vec<&RestoreCandidate> {
    candidates
        .iter()
        .filter(|c| c.run_id == Some(run_id))
        .collect()
}

/// 復元操作全体の結果。
#[derive(Debug)]
pub struct RestoreOutcome {
    /// 項目ごとの結果（元のパスと結果の組）。
    pub results: Vec<(PathBuf, RestoreItemOutcome)>,
}

impl RestoreOutcome {
    /// 復元に成功した件数。
    pub fn restored_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, o)| *o == RestoreItemOutcome::Restored)
            .count()
    }

    /// 衝突によりスキップした件数。
    pub fn skipped_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, o)| *o == RestoreItemOutcome::SkippedCollision)
            .count()
    }

    /// 見つからなかった件数。
    pub fn not_found_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, o)| *o == RestoreItemOutcome::NotFound)
            .count()
    }

    /// 復元に失敗した件数。
    pub fn failed_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, o)| matches!(o, RestoreItemOutcome::Failed { .. }))
            .count()
    }
}

/// `candidates` を実際に復元する。[`Platform::restore_from_trash`] を呼ぶ
/// だけで、削除（[`crate::delete::DeletePlan`] / [`crate::delete::execute`]）
/// には一切触れない（モジュール doc 参照）。
pub fn restore(platform: &dyn Platform, candidates: &[RestoreCandidate]) -> RestoreOutcome {
    let ids: Vec<String> = candidates.iter().map(|c| c.entry.id.clone()).collect();
    let path_by_id: HashMap<String, PathBuf> = candidates
        .iter()
        .map(|c| (c.entry.id.clone(), c.entry.original_path.clone()))
        .collect();

    let outcomes = platform.restore_from_trash(&ids);
    let results = outcomes
        .into_iter()
        .map(|(id, outcome)| {
            let path = path_by_id.get(&id).cloned().unwrap_or_default();
            (path, outcome)
        })
        .collect();

    RestoreOutcome { results }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delete::DeleteMethod;
    use crate::platform::{ElevateError, ElevateResult, KnownDir, PlatformError, Result};
    use std::path::Path;

    fn trash_entry(id: &str, original_path: &str, deleted_at_secs: Option<u64>) -> TrashEntry {
        TrashEntry {
            id: id.to_string(),
            original_path: PathBuf::from(original_path),
            deleted_at_secs,
            size: Some(10),
        }
    }

    fn audit_record(run_id: u64, timestamp_secs: u64, path: &str, rule_id: &str) -> AuditRecord {
        AuditRecord {
            run_id,
            timestamp_secs,
            rule_id: rule_id.to_string(),
            path: PathBuf::from(path),
            size: 10,
            action: DeleteAction::ToTrash,
            outcome: ItemOutcome::Deleted,
            method: DeleteMethod::Trash,
        }
    }

    // ---- correlate ----

    #[test]
    fn correlate_matches_by_path_action_and_outcome() {
        let trash = vec![trash_entry("1", "/tmp/a.txt", Some(100))];
        let audit = vec![audit_record(7, 90, "/tmp/a.txt", "user_temp")];
        let candidates = correlate(&trash, &audit);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rule_id.as_deref(), Some("user_temp"));
        assert_eq!(candidates[0].run_id, Some(7));
    }

    #[test]
    fn correlate_ignores_records_with_different_action_or_outcome() {
        let trash = vec![trash_entry("1", "/tmp/a.txt", Some(100))];
        let mut permanent = audit_record(1, 90, "/tmp/a.txt", "user_temp");
        permanent.action = DeleteAction::RemovePath;
        let mut failed = audit_record(2, 95, "/tmp/a.txt", "user_temp");
        failed.outcome = ItemOutcome::Failed {
            message: "boom".to_string(),
        };
        let candidates = correlate(&trash, &[permanent, failed]);
        assert_eq!(
            candidates[0].rule_id, None,
            "action/outcome が違えば無視する"
        );
        assert_eq!(candidates[0].run_id, None);
    }

    #[test]
    fn correlate_leaves_unmatched_trash_entries_with_none() {
        let trash = vec![trash_entry("1", "/tmp/untracked.txt", Some(100))];
        let audit = vec![audit_record(1, 90, "/tmp/other.txt", "user_temp")];
        let candidates = correlate(&trash, &audit);
        assert_eq!(
            candidates[0].rule_id, None,
            "他アプリが消したものは由来不明"
        );
        assert_eq!(candidates[0].run_id, None);
    }

    #[test]
    fn correlate_handles_empty_audit_log() {
        let trash = vec![trash_entry("1", "/tmp/a.txt", Some(100))];
        let candidates = correlate(&trash, &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rule_id, None);
    }

    #[test]
    fn correlate_picks_the_closest_deletion_time_when_path_deleted_multiple_times() {
        let trash = vec![trash_entry("1", "/tmp/a.txt", Some(1000))];
        let audit = vec![
            audit_record(1, 500, "/tmp/a.txt", "old_run"),
            audit_record(2, 990, "/tmp/a.txt", "closest_run"),
        ];
        let candidates = correlate(&trash, &audit);
        assert_eq!(candidates[0].rule_id.as_deref(), Some("closest_run"));
        assert_eq!(candidates[0].run_id, Some(2));
    }

    #[test]
    fn correlate_picks_the_newest_record_when_deleted_at_secs_is_unavailable() {
        let trash = vec![trash_entry("1", "/tmp/a.txt", None)];
        let audit = vec![
            audit_record(1, 500, "/tmp/a.txt", "older"),
            audit_record(2, 900, "/tmp/a.txt", "newer"),
        ];
        let candidates = correlate(&trash, &audit);
        assert_eq!(candidates[0].rule_id.as_deref(), Some("newer"));
    }

    // ---- candidates_for_run ----

    #[test]
    fn candidates_for_run_filters_by_run_id() {
        let candidates = vec![
            RestoreCandidate {
                entry: trash_entry("1", "/tmp/a.txt", None),
                rule_id: Some("r".to_string()),
                run_id: Some(1),
            },
            RestoreCandidate {
                entry: trash_entry("2", "/tmp/b.txt", None),
                rule_id: Some("r".to_string()),
                run_id: Some(2),
            },
        ];
        let filtered = candidates_for_run(&candidates, 1);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].entry.id, "1");
    }

    // ---- restore ----

    struct FakePlatform {
        outcomes: HashMap<String, RestoreItemOutcome>,
    }

    impl Platform for FakePlatform {
        fn known_dir(&self, _kind: KnownDir) -> Option<PathBuf> {
            None
        }
        fn to_trash(&self, _path: &Path) -> Result<()> {
            Err(PlatformError::Unsupported("to_trash"))
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
        fn restore_from_trash(&self, ids: &[String]) -> Vec<(String, RestoreItemOutcome)> {
            ids.iter()
                .map(|id| {
                    let outcome = self
                        .outcomes
                        .get(id)
                        .cloned()
                        .unwrap_or(RestoreItemOutcome::NotFound);
                    (id.clone(), outcome)
                })
                .collect()
        }
    }

    #[test]
    fn restore_aggregates_mixed_outcomes_and_maps_back_to_paths() {
        let candidates = vec![
            RestoreCandidate {
                entry: trash_entry("1", "/tmp/ok.txt", None),
                rule_id: Some("r".to_string()),
                run_id: Some(1),
            },
            RestoreCandidate {
                entry: trash_entry("2", "/tmp/collide.txt", None),
                rule_id: Some("r".to_string()),
                run_id: Some(1),
            },
            RestoreCandidate {
                entry: trash_entry("3", "/tmp/gone.txt", None),
                rule_id: Some("r".to_string()),
                run_id: Some(1),
            },
        ];
        let mut outcomes = HashMap::new();
        outcomes.insert("1".to_string(), RestoreItemOutcome::Restored);
        outcomes.insert("2".to_string(), RestoreItemOutcome::SkippedCollision);
        // "3" is intentionally absent -> NotFound（既定値）。
        let platform = FakePlatform { outcomes };

        let outcome = restore(&platform, &candidates);
        assert_eq!(outcome.restored_count(), 1);
        assert_eq!(outcome.skipped_count(), 1);
        assert_eq!(outcome.not_found_count(), 1);
        assert_eq!(outcome.failed_count(), 0);

        let ok = outcome
            .results
            .iter()
            .find(|(path, _)| path == Path::new("/tmp/ok.txt"))
            .unwrap();
        assert_eq!(ok.1, RestoreItemOutcome::Restored);
    }

    #[test]
    fn restore_does_not_stop_on_a_single_failure() {
        let candidates = vec![
            RestoreCandidate {
                entry: trash_entry("1", "/tmp/ok.txt", None),
                rule_id: None,
                run_id: None,
            },
            RestoreCandidate {
                entry: trash_entry("2", "/tmp/bad.txt", None),
                rule_id: None,
                run_id: None,
            },
        ];
        let mut outcomes = HashMap::new();
        outcomes.insert("1".to_string(), RestoreItemOutcome::Restored);
        outcomes.insert(
            "2".to_string(),
            RestoreItemOutcome::Failed {
                message: "boom".to_string(),
            },
        );
        let platform = FakePlatform { outcomes };

        let outcome = restore(&platform, &candidates);
        assert_eq!(outcome.restored_count(), 1);
        assert_eq!(outcome.failed_count(), 1);
    }
}
