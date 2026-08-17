//! 表示用の純粋ロジック。判定・削除ロジックは持たない（F-GUI-07）。
//!
//! `egui` に依存しない形にすることで、GUI を実際に起動せずにテストできる
//! （`platform/windows.rs` が純粋層とバインディング層に分かれているのと
//! 同じ考え方）。

use pc_cleaner_core::rule::builtin_rules;
use pc_cleaner_core::{Config, Rule, Safety, ScanEntry, SkipReason, apply_rule_prefs};
use std::collections::HashMap;
use std::path::PathBuf;

/// CLI/GUI 共通の整形ロジック（core に集約済み）をそのまま使う。
pub use pc_cleaner_core::human_size;

/// 走査範囲。フロー①（ワンクリック掃除）とフロー②（手動レビュー）の切り替え。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Safe ルールのみ（フロー①）。
    SafeOnly,
    /// Safe / Caution / Review すべて（フロー②）。
    All,
}

impl Scope {
    /// core の `safety_scope(all)` に委譲する（F-CLI-08 / 9.5：CLI の
    /// `--all` フラグと同じ `Vec<Safety>` の組み立てを1箇所に保つ）。
    pub fn safeties(self) -> Vec<Safety> {
        pc_cleaner_core::safety_scope(matches!(self, Scope::All))
    }

    /// 表示ラベル。
    pub fn label(self) -> &'static str {
        match self {
            Scope::SafeOnly => "安全なものだけ",
            Scope::All => "すべて表示",
        }
    }
}

/// `Safety` の表示ラベル（F-GUI-03：安全度に応じた表示区別）。
pub fn safety_label(safety: Safety) -> &'static str {
    match safety {
        Safety::Safe => "安全",
        Safety::Caution => "注意",
        Safety::Review => "要確認",
    }
}

/// 経過日数の表示ラベル。
pub fn age_label(age_days: Option<u64>) -> String {
    match age_days {
        Some(0) => "本日".to_string(),
        Some(n) => format!("{n}日"),
        None => "不明".to_string(),
    }
}

/// `needs_admin` なルール（`system_temp`）のみを返す。走査対象には含まれない
/// ため、一覧に「将来対応」として別枠表示するために使う（NF-SAF-05）。
pub fn future_rules() -> Vec<Rule> {
    builtin_rules()
        .into_iter()
        .filter(|r| r.needs_admin)
        .collect()
}

/// `SkipReason` の表示ラベル。
pub fn skip_reason_label(reason: &SkipReason) -> &'static str {
    match reason {
        SkipReason::NeedsAdmin => "管理者権限が必要なため対象外（将来対応）",
        SkipReason::UnknownBase => "この環境では場所を特定できませんでした",
        SkipReason::Unreadable => "読み取れませんでした",
        SkipReason::MissingThreshold => "しきい値が未設定のため対象外",
    }
}

/// `rule_id` が一致するエントリだけに `config.rule_prefs` を反映し直す。
///
/// `apply_rule_prefs` をエントリ全体に掛けると、他ルールでのユーザーの手動
/// 選択が失われるため、対象ルールの範囲だけに絞って適用する（判定自体は
/// `apply_rule_prefs`＝core が行い、GUI は範囲を絞るだけ）。
pub fn reapply_pref_for_rule(entries: &mut [ScanEntry], rule_id: &str, config: &Config) {
    let mut subset: Vec<ScanEntry> = entries
        .iter()
        .filter(|e| e.rule_id == rule_id)
        .cloned()
        .collect();
    apply_rule_prefs(&mut subset, config);
    let mut updated = subset.into_iter();
    for entry in entries.iter_mut().filter(|e| e.rule_id == rule_id) {
        if let Some(u) = updated.next() {
            entry.selected = u.selected;
        }
    }
}

/// 除外された行の表示用メッセージ（`ExclusionReason` の一部のみ GUI で使う。
/// `NotSelected` は「チェックが外れている」という表示そのものなので対象外）。
pub fn recycle_bin_exclusion_hint() -> &'static str {
    "ゴミ箱の中身は「完全削除」にチェックを入れた場合のみ対象になります（復元できません）。"
}

/// 削除計画から除外されたパスの一覧（NotSelected を除く）を、UI が
/// バッジ表示に使えるマップへ変換する。
pub fn to_exclusion_map<I>(excluded: I) -> HashMap<PathBuf, &'static str>
where
    I: IntoIterator<Item = (PathBuf, pc_cleaner_core::ExclusionReason)>,
{
    excluded
        .into_iter()
        .filter_map(|(path, reason)| exclusion_label(reason).map(|label| (path, label)))
        .collect()
}

/// `ExclusionReason` の表示ラベル。`NotSelected` は通常表示（未選択）なので
/// バッジ化しない。
pub fn exclusion_label(reason: pc_cleaner_core::ExclusionReason) -> Option<&'static str> {
    use pc_cleaner_core::ExclusionReason as R;
    match reason {
        R::NotSelected => None,
        R::UnknownRule => Some("許可リスト外のため対象外"),
        R::NeedsAdmin => Some("管理者権限が必要なため対象外"),
        R::OutsideRuleBase => Some("走査基点の外のため対象外"),
        R::UnresolvedBase => Some("場所を特定できないため対象外"),
        R::RequiresPermanent => Some("完全削除が必要（ゴミ箱の中身）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_cleaner_core::RulePref;

    #[test]
    fn scope_safeties_matches_cli_safety_scope() {
        assert_eq!(Scope::SafeOnly.safeties(), vec![Safety::Safe]);
        assert_eq!(
            Scope::All.safeties(),
            vec![Safety::Safe, Safety::Caution, Safety::Review]
        );
    }

    #[test]
    fn safety_label_covers_all_variants() {
        assert_eq!(safety_label(Safety::Safe), "安全");
        assert_eq!(safety_label(Safety::Caution), "注意");
        assert_eq!(safety_label(Safety::Review), "要確認");
    }

    #[test]
    fn age_label_formats_known_cases() {
        assert_eq!(age_label(Some(0)), "本日");
        assert_eq!(age_label(Some(5)), "5日");
        assert_eq!(age_label(None), "不明");
    }

    #[test]
    fn future_rules_contains_only_needs_admin_rules() {
        let rules = future_rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "system_temp");
        assert!(rules[0].needs_admin);
    }

    #[test]
    fn skip_reason_label_covers_all_variants() {
        for reason in [
            SkipReason::NeedsAdmin,
            SkipReason::UnknownBase,
            SkipReason::Unreadable,
            SkipReason::MissingThreshold,
        ] {
            assert!(!skip_reason_label(&reason).is_empty());
        }
    }

    #[test]
    fn exclusion_label_covers_all_variants_except_not_selected() {
        use pc_cleaner_core::ExclusionReason as R;
        assert_eq!(exclusion_label(R::NotSelected), None);
        for reason in [
            R::UnknownRule,
            R::NeedsAdmin,
            R::OutsideRuleBase,
            R::UnresolvedBase,
            R::RequiresPermanent,
        ] {
            assert!(exclusion_label(reason).is_some());
        }
    }

    fn entry(rule_id: &str, selected: bool, recommended: bool) -> ScanEntry {
        ScanEntry {
            rule_id: rule_id.to_string(),
            path: PathBuf::from(format!("/{rule_id}")),
            size: 0,
            file_count: 1,
            modified: None,
            age_days: None,
            recommended,
            reason: String::new(),
            selected,
        }
    }

    #[test]
    fn reapply_pref_for_rule_only_touches_the_target_rule() {
        let mut entries = vec![
            entry("a", true, true),
            entry("b", true, true), // 手動で選択したままにしたい
        ];
        let mut config = Config::default();
        config.rule_prefs.insert("a".to_string(), RulePref::Exclude);

        reapply_pref_for_rule(&mut entries, "a", &config);

        assert!(!entries[0].selected, "対象ルールには反映される");
        assert!(entries[1].selected, "他ルールの選択は変わらない");
    }
}
