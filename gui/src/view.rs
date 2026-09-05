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

/// エントリのサイズが `threshold` 以上のとき、表示用のバッジ文言を返す
/// （C2 / Issue #48）。`threshold` が `None`（そのルールでは大容量注意を
/// 出さない）のときは常に `None`。
pub fn large_file_badge(size: u64, threshold: Option<u64>) -> Option<String> {
    let threshold = threshold?;
    (size >= threshold).then(|| format!("⚠ 大容量（{}）", human_size(size)))
}

/// エントリが重複ファイルグループに属するとき、表示用のバッジ文言を返す
/// （C2 / Issue #48）。グループの代表（`is_primary`）には出さない：削除して
/// よいのは「他に残っている」側であることを示すのが目的で、代表自身に注意書き
/// を出すと紛らわしいため。
pub fn duplicate_badge(duplicate: Option<pc_cleaner_core::DuplicateInfo>) -> Option<String> {
    let info = duplicate?;
    if info.is_primary {
        return None;
    }
    Some(format!("⧉ 重複（他に{}件）", info.group_size - 1))
}

/// 現在の昇格状態では対象にできない（`needs_admin` かつ未昇格の）ルールを
/// 返す。一覧に「管理者権限が必要」として別枠表示するために使う
/// （NF-SAF-05 / A1 / Issue #40）。
///
/// [`pc_cleaner_core::rule::scannable_rules`] の厳密な補集合であること
/// （`future_rules_and_scannable_rules_are_exact_complements` で検証）。
pub fn future_rules(config: &Config, elevated: bool) -> Vec<Rule> {
    builtin_rules(config)
        .into_iter()
        .filter(|r| !r.is_permitted(elevated))
        .collect()
}

/// `SkipReason` の表示ラベル。
pub fn skip_reason_label(reason: &SkipReason) -> &'static str {
    match reason {
        SkipReason::NeedsAdmin => {
            "管理者権限が必要なため対象外（管理者として実行し直すと対象になります）"
        }
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

/// 昇格後プロセスへ渡す起動引数を作る（A2 / Issue #41）。
///
/// `current`（`std::env::args().skip(1)` 相当）をそのまま引き継ぎ、内部用
/// マーカー `--elevated` を末尾に足す。既に付いている場合は重複させない
/// （二重防御：万一 `elevate()` が誤って多重に呼ばれても引数が増え続けない）。
pub fn relaunch_args(current: &[String]) -> Vec<String> {
    let mut args = current.to_vec();
    if !args.iter().any(|a| a == "--elevated") {
        args.push("--elevated".to_string());
    }
    args
}

/// 昇格確認モーダルの案内文。
pub fn elevate_confirm_text() -> &'static str {
    "pc-cleaner を管理者権限で起動し直します。\n\
     UAC の確認画面で「はい」を選択してください。\n\n\
     現在のウィンドウは終了し、走査結果と未保存の選択は失われます\
     （ルール別の既定は保存してから起動し直します）。"
}

/// 削除失敗を message ごとにグルーピングする。多くの場合、失敗は共通の
/// 原因（同じメッセージ）に集約されるため、1 ファイル 1 行の羅列ではなく
/// 「メッセージ（件数）」単位にまとめて表示する（#35）。
///
/// 件数の多いグループを先に表示する。
pub fn group_failures(failures: &[(PathBuf, String)]) -> Vec<(String, Vec<PathBuf>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for (path, message) in failures {
        groups
            .entry(message.clone())
            .or_insert_with(|| {
                order.push(message.clone());
                Vec::new()
            })
            .push(path.clone());
    }
    let mut result: Vec<(String, Vec<PathBuf>)> = order
        .into_iter()
        .map(|message| (message.clone(), groups.remove(&message).unwrap_or_default()))
        .collect();
    result.sort_by_key(|(_, paths)| std::cmp::Reverse(paths.len()));
    result
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
    fn large_file_badge_is_none_without_a_threshold() {
        assert_eq!(large_file_badge(1_000_000_000, None), None);
    }

    #[test]
    fn large_file_badge_appears_at_or_above_threshold() {
        assert_eq!(large_file_badge(999, Some(1_000)), None);
        assert!(large_file_badge(1_000, Some(1_000)).is_some());
        assert!(large_file_badge(2_000, Some(1_000)).is_some());
    }

    #[test]
    fn duplicate_badge_is_none_for_primary_or_missing() {
        use pc_cleaner_core::DuplicateInfo;
        assert_eq!(duplicate_badge(None), None);
        assert_eq!(
            duplicate_badge(Some(DuplicateInfo {
                group_id: 0,
                group_size: 3,
                is_primary: true,
            })),
            None
        );
    }

    #[test]
    fn duplicate_badge_shows_remaining_count_for_non_primary() {
        use pc_cleaner_core::DuplicateInfo;
        let badge = duplicate_badge(Some(DuplicateInfo {
            group_id: 0,
            group_size: 3,
            is_primary: false,
        }))
        .unwrap();
        assert!(badge.contains('2'));
    }

    #[test]
    fn future_rules_contains_only_needs_admin_rules_when_not_elevated() {
        let rules = future_rules(&Config::default(), false);
        assert!(!rules.is_empty());
        assert!(rules.iter().all(|r| r.needs_admin));
        assert!(rules.iter().any(|r| r.id == "system_temp"));
        assert!(rules.iter().any(|r| r.id == "windows_update_cache"));
        assert!(rules.iter().any(|r| r.id == "delivery_optimization_cache"));
    }

    #[test]
    fn future_rules_is_empty_when_elevated() {
        assert!(future_rules(&Config::default(), true).is_empty());
    }

    #[test]
    fn future_rules_and_scannable_rules_are_exact_complements() {
        use pc_cleaner_core::rule::{builtin_rules, scannable_rules};

        let config = Config::default();
        for elevated in [false, true] {
            let future = future_rules(&config, elevated);
            let scannable = scannable_rules(&config, elevated);
            assert_eq!(
                future.len() + scannable.len(),
                builtin_rules(&config).len(),
                "elevated={elevated}: future_rules と scannable_rules の和が全ルール数と一致すること"
            );
            for rule in &future {
                assert!(
                    !scannable.iter().any(|r| r.id == rule.id),
                    "elevated={elevated}: {} が両方に含まれている",
                    rule.id
                );
            }
        }
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
            in_use: None,
            duplicate: None,
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

    #[test]
    fn group_failures_groups_by_message_and_sorts_by_count_desc() {
        let failures = vec![
            (PathBuf::from("/a"), "使用中".to_string()),
            (PathBuf::from("/b"), "権限不足".to_string()),
            (PathBuf::from("/c"), "使用中".to_string()),
            (PathBuf::from("/d"), "使用中".to_string()),
        ];
        let groups = group_failures(&failures);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "使用中");
        assert_eq!(groups[0].1.len(), 3);
        assert_eq!(groups[1].0, "権限不足");
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn group_failures_handles_empty_input() {
        assert!(group_failures(&[]).is_empty());
    }

    #[test]
    fn relaunch_args_appends_elevated_marker() {
        let current = vec!["--demo".to_string()];
        assert_eq!(
            relaunch_args(&current),
            vec!["--demo".to_string(), "--elevated".to_string()]
        );
    }

    #[test]
    fn relaunch_args_does_not_duplicate_the_marker() {
        let current = vec!["--elevated".to_string()];
        assert_eq!(relaunch_args(&current), vec!["--elevated".to_string()]);
    }

    #[test]
    fn relaunch_args_handles_empty_input() {
        assert_eq!(relaunch_args(&[]), vec!["--elevated".to_string()]);
    }

    #[test]
    fn elevate_confirm_text_is_non_empty() {
        assert!(!elevate_confirm_text().is_empty());
    }
}
