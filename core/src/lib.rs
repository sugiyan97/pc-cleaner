//! `pc-cleaner` のロジック本体（UI 非依存）。
//!
//! Windows を優先しつつ OS 依存を隔離した、手動選択型のディスク掃除ツール
//! `pc-cleaner` の中核ライブラリ。走査・判定支援・削除・設定のロジックを
//! ここに集約し、`cli` / `gui` は表示と操作のみを担う（設計目標 G4）。
//! OS 固有の知識は [`platform`] の裏に閉じ込める（設計目標 G3）。
//!
//! 詳細は `docs/requirements.md` を参照。

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub mod audit;
pub mod breakdown;
pub mod config;
pub mod delete;
pub mod entry;
pub mod format;
pub mod history;
pub mod inspect;
pub mod platform;
pub mod recommend;
pub mod rule;
pub mod scan;

pub use audit::AuditRecord;
pub use breakdown::{BreakdownItem, BucketBreakdown, CategoryBreakdown, SizeBucket};
pub use config::{Config, RulePref};
pub use delete::{
    DeleteAction, DeleteMethod, DeleteMode, DeleteOutcome, DeletePlan, DeleteProgress,
    DeleteRequest, DryRunReason, ExcludedEntry, ExclusionReason, ItemOutcome, ItemResult,
    PlannedDeletion, execute, execute_with_progress, preview,
};
pub use entry::{DuplicateInfo, ScanEntry};
pub use format::human_size;
pub use history::{History, HistoryEntry};
pub use platform::{ElevateError, ElevateResult, KnownDir, should_relaunch};
pub use recommend::Recommendation;
pub use rule::{MatchKind, Rule, Safety};
pub use scan::{
    ScanProgress, SkipReason, apply_rule_prefs, rules_for_safeties, safety_scope, scan,
    scan_pipeline, scan_pipeline_with_progress, scan_with_progress,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn core_model_types_are_send_and_sync() {
        // F-SCAN-05 / NF-PRF-01（基点ごとの並列走査）の前提を先に固定する。
        assert_send_sync::<Rule>();
        assert_send_sync::<ScanEntry>();
        assert_send_sync::<Safety>();
        assert_send_sync::<KnownDir>();
    }

    #[test]
    fn rule_and_scan_entry_compose_and_round_trip() {
        let rule = Rule {
            id: "old_logs".to_string(),
            label: "古いログ".to_string(),
            description: "180日超のログファイル".to_string(),
            base: KnownDir::LocalAppData,
            match_kind: MatchKind::Extension(vec!["log".to_string(), "tmp".to_string()]),
            needs_admin: false,
            safety: Safety::Caution,
            age_threshold_days: Some(180),
            large_file_threshold_bytes: None,
        };

        let entry = ScanEntry {
            rule_id: rule.id.clone(),
            path: "/tmp/app/old.log".into(),
            size: 1024,
            file_count: 1,
            modified: None,
            age_days: Some(200),
            in_use: None,
            duplicate: None,
            recommended: true,
            reason: "180日以上経過".to_string(),
            selected: false,
        };

        let cloned = entry.clone();
        assert_eq!(entry, cloned);

        let extensions: HashSet<&str> = match &rule.match_kind {
            MatchKind::Extension(exts) => exts.iter().map(String::as_str).collect(),
            _ => HashSet::new(),
        };
        assert!(extensions.contains("log"));
    }
}
