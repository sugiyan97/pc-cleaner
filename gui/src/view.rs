//! 表示用の純粋ロジック。判定・削除ロジックは持たない（F-GUI-07）。
//!
//! `egui` に依存しない形にすることで、GUI を実際に起動せずにテストできる
//! （`platform/windows.rs` が純粋層とバインディング層に分かれているのと
//! 同じ考え方）。

use pc_cleaner_core::format::relative_days;
use pc_cleaner_core::{
    AuditRecord, BucketBreakdown, CategoryBreakdown, Config, History, ItemOutcome, Lang, Rule,
    Safety, ScanEntry, SkipReason, apply_rule_prefs,
};
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
    pub fn label(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (Scope::SafeOnly, Lang::Ja) => "安全なものだけ",
            (Scope::All, Lang::Ja) => "すべて表示",
            (Scope::SafeOnly, Lang::En) => "Safe items only",
            (Scope::All, Lang::En) => "Show all",
        }
    }
}

/// `Safety` の表示ラベル（F-GUI-03：安全度に応じた表示区別）。
pub fn safety_label(safety: Safety, lang: Lang) -> &'static str {
    match (safety, lang) {
        (Safety::Safe, Lang::Ja) => "安全",
        (Safety::Caution, Lang::Ja) => "注意",
        (Safety::Review, Lang::Ja) => "要確認",
        (Safety::Safe, Lang::En) => "Safe",
        (Safety::Caution, Lang::En) => "Caution",
        (Safety::Review, Lang::En) => "Review",
    }
}

/// 経過日数の表示ラベル。
pub fn age_label(age_days: Option<u64>, lang: Lang) -> String {
    match (age_days, lang) {
        (Some(0), Lang::Ja) => "本日".to_string(),
        (Some(0), Lang::En) => "Today".to_string(),
        (Some(n), Lang::Ja) => format!("{n}日"),
        (Some(n), Lang::En) => format!("{n} days"),
        (None, Lang::Ja) => "不明".to_string(),
        (None, Lang::En) => "Unknown".to_string(),
    }
}

/// エントリのサイズが `threshold` 以上のとき、表示用のバッジ文言を返す
/// （C2 / Issue #48）。`threshold` が `None`（そのルールでは大容量注意を
/// 出さない）のときは常に `None`。
pub fn large_file_badge(size: u64, threshold: Option<u64>, lang: Lang) -> Option<String> {
    let threshold = threshold?;
    (size >= threshold).then(|| match lang {
        Lang::Ja => format!("⚠ 大容量（{}）", human_size(size)),
        Lang::En => format!("⚠ Large ({})", human_size(size)),
    })
}

/// エントリが重複ファイルグループに属するとき、表示用のバッジ文言を返す
/// （C2 / Issue #48）。グループの代表（`is_primary`）には出さない：削除して
/// よいのは「他に残っている」側であることを示すのが目的で、代表自身に注意書き
/// を出すと紛らわしいため。
pub fn duplicate_badge(
    duplicate: Option<pc_cleaner_core::DuplicateInfo>,
    lang: Lang,
) -> Option<String> {
    let info = duplicate?;
    if info.is_primary {
        return None;
    }
    Some(match lang {
        Lang::Ja => format!("⧉ 重複（他に{}件）", info.group_size - 1),
        Lang::En => format!("⧉ Duplicate ({} others)", info.group_size - 1),
    })
}

/// `rules`（走査 scope に絞り込む前の解決済み全ルール、`RuleSet::all()`）の
/// うち、現在の昇格状態では対象にできない（`needs_admin` かつ未昇格の）もの
/// を返す。一覧に「管理者権限が必要」として別枠表示するために使う
/// （NF-SAF-05 / A1 / Issue #40）。
///
/// [`pc_cleaner_core::rule::scannable_rules`] の厳密な補集合であること
/// （`future_rules_and_scannable_rules_are_exact_complements` で検証）。
pub fn future_rules(rules: &[Rule], elevated: bool) -> Vec<Rule> {
    rules
        .iter()
        .filter(|r| !r.is_permitted(elevated))
        .cloned()
        .collect()
}

/// `SkipReason` の表示ラベル。
pub fn skip_reason_label(reason: &SkipReason, lang: Lang) -> &'static str {
    match (reason, lang) {
        (SkipReason::NeedsAdmin, Lang::Ja) => {
            "管理者権限が必要なため対象外（管理者として実行し直すと対象になります）"
        }
        (SkipReason::UnknownBase, Lang::Ja) => "この環境では場所を特定できませんでした",
        (SkipReason::Unreadable, Lang::Ja) => "読み取れませんでした",
        (SkipReason::MissingThreshold, Lang::Ja) => "しきい値が未設定のため対象外",
        (SkipReason::NeedsAdmin, Lang::En) => {
            "Excluded because administrator privileges are required (available if you \
            restart as administrator)"
        }
        (SkipReason::UnknownBase, Lang::En) => "Could not determine the location on this system",
        (SkipReason::Unreadable, Lang::En) => "Could not be read",
        (SkipReason::MissingThreshold, Lang::En) => "Excluded because the threshold is not set",
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
pub fn recycle_bin_exclusion_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::Ja => {
            "ゴミ箱の中身は「完全削除」にチェックを入れた場合のみ対象になります（復元できません）。"
        }
        Lang::En => {
            "Items already in the Recycle Bin are only included when \"Permanently delete\" \
            is checked (cannot be restored)."
        }
    }
}

/// 削除計画から除外されたパスの一覧（NotSelected を除く）を、UI が
/// バッジ表示に使えるマップへ変換する。
pub fn to_exclusion_map<I>(excluded: I, lang: Lang) -> HashMap<PathBuf, &'static str>
where
    I: IntoIterator<Item = (PathBuf, pc_cleaner_core::ExclusionReason)>,
{
    excluded
        .into_iter()
        .filter_map(|(path, reason)| exclusion_label(reason, lang).map(|label| (path, label)))
        .collect()
}

/// `ExclusionReason` の表示ラベル。`NotSelected` は通常表示（未選択）なので
/// バッジ化しない。
pub fn exclusion_label(
    reason: pc_cleaner_core::ExclusionReason,
    lang: Lang,
) -> Option<&'static str> {
    use pc_cleaner_core::ExclusionReason as R;
    match (reason, lang) {
        (R::NotSelected, _) => None,
        (R::UnknownRule, Lang::Ja) => Some("許可リスト外のため対象外"),
        (R::NeedsAdmin, Lang::Ja) => Some("管理者権限が必要なため対象外"),
        (R::OutsideRuleBase, Lang::Ja) => Some("走査基点の外のため対象外"),
        (R::UnresolvedBase, Lang::Ja) => Some("場所を特定できないため対象外"),
        (R::RequiresPermanent, Lang::Ja) => Some("完全削除が必要（ゴミ箱の中身）"),
        (R::UnknownRule, Lang::En) => Some("Excluded: not in the rule allowlist"),
        (R::NeedsAdmin, Lang::En) => Some("Excluded: administrator privileges required"),
        (R::OutsideRuleBase, Lang::En) => Some("Excluded: outside the rule's scan base"),
        (R::UnresolvedBase, Lang::En) => Some("Excluded: could not determine the location"),
        (R::RequiresPermanent, Lang::En) => {
            Some("Requires permanent deletion (already in Recycle Bin)")
        }
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
pub fn elevate_confirm_text(lang: Lang) -> &'static str {
    match lang {
        Lang::Ja => {
            "pc-cleaner を管理者権限で起動し直します。\n\
             UAC の確認画面で「はい」を選択してください。\n\n\
             現在のウィンドウは終了し、走査結果と未保存の選択は失われます\
             （ルール別の既定は保存してから起動し直します）。"
        }
        Lang::En => {
            "pc-cleaner will restart with administrator privileges.\n\
             Please select \"Yes\" on the UAC prompt.\n\n\
             The current window will close, and scan results and unsaved selections \
             will be lost (rule defaults are saved before restarting)."
        }
    }
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

/// 内訳1行分の表示用データ（C3：プレビュー時の内訳表示）。`egui` に依存せず、
/// バーの長さは `fraction`（0.0〜1.0）として渡すだけにする（実際の
/// `ProgressBar` 描画は `app.rs` が行う）。
pub struct BreakdownRow {
    /// 表示名（ルールラベルまたはサイズ帯ラベル）。
    pub label: String,
    /// 件数・サイズを添えた補足テキスト。
    pub detail: String,
    /// 全体に対する比率（0.0〜1.0）。`total_size == 0` のときは 0.0。
    pub fraction: f32,
}

/// 種類別（ルール別）の内訳行を作る。`rules` にラベルが無いルール ID は、
/// `ui_central` の行表示（app.rs 462行目付近）と同じフォールバックで
/// `rule_id` をそのまま表示名にする。
pub fn category_rows(
    breakdown: &[CategoryBreakdown],
    rules: &HashMap<String, Rule>,
    total_size: u64,
    lang: Lang,
) -> Vec<BreakdownRow> {
    breakdown
        .iter()
        .map(|b| {
            let label = rules
                .get(&b.rule_id)
                .map(|r| r.label.clone())
                .unwrap_or_else(|| b.rule_id.clone());
            BreakdownRow {
                label,
                detail: match lang {
                    Lang::Ja => format!("{} 件 / {}", b.item_count, human_size(b.total_size)),
                    Lang::En => format!("{} items / {}", b.item_count, human_size(b.total_size)),
                },
                fraction: if total_size == 0 {
                    0.0
                } else {
                    b.total_size as f32 / total_size as f32
                },
            }
        })
        .collect()
}

/// サイズ帯別の内訳行を作る。空の帯（`item_count == 0`）は表示層で除く
/// （core の `by_size_bucket` は合計の完全性を保つため常に全帯を返すが、
/// 表示上は該当なしの帯を並べても意味がないため）。
pub fn bucket_rows(
    breakdown: &[BucketBreakdown],
    total_size: u64,
    lang: Lang,
) -> Vec<BreakdownRow> {
    breakdown
        .iter()
        .filter(|b| b.item_count > 0)
        .map(|b| BreakdownRow {
            label: b.bucket.label(lang).to_string(),
            detail: match lang {
                Lang::Ja => format!("{} 件 / {}", b.item_count, human_size(b.total_size)),
                Lang::En => format!("{} items / {}", b.item_count, human_size(b.total_size)),
            },
            fraction: if total_size == 0 {
                0.0
            } else {
                b.total_size as f32 / total_size as f32
            },
        })
        .collect()
}

/// 履歴一覧の1行分の表示用データ（C4 / Issue #50）。
pub struct HistoryRow {
    /// 実行時刻の相対表現（例: "3日前"）。
    pub when: String,
    /// 解放容量・件数のまとめ。
    pub summary: String,
    /// この実行の識別子。ロールバック補助（D3 / Issue #53）が「この実行を
    /// まとめて元に戻す」ボタンの対象特定に使う。`0` は #51 より前に記録
    /// された実績で対応する `run_id` が無いことを表す（`HistoryEntry::run_id`
    /// のドキュメント参照）。
    pub run_id: u64,
}

/// `history` から、新しい順に最大 `limit` 件の表示行を作る。
///
/// `now_secs`（UNIX epoch 秒）は呼び出し側（GUI）が渡す：本モジュールは
/// `egui` 非依存の純粋ロジックに保つため、現在時刻を自ら参照しない
/// （`recommend.rs` が現在時刻を参照しないのと同じ方針）。
pub fn history_rows(history: &History, now_secs: u64, limit: usize, lang: Lang) -> Vec<HistoryRow> {
    history
        .recent(limit)
        .into_iter()
        .map(|entry| {
            let seconds_ago = now_secs.saturating_sub(entry.timestamp_secs);
            HistoryRow {
                when: relative_days(seconds_ago, lang),
                summary: match lang {
                    Lang::Ja => format!(
                        "{} 解放 / {} 件{}",
                        human_size(entry.freed_bytes),
                        entry.deleted_count,
                        if entry.permanent {
                            "（完全削除）"
                        } else {
                            ""
                        }
                    ),
                    Lang::En => format!(
                        "{} freed / {} items{}",
                        human_size(entry.freed_bytes),
                        entry.deleted_count,
                        if entry.permanent {
                            " (permanently deleted)"
                        } else {
                            ""
                        }
                    ),
                },
                run_id: entry.run_id,
            }
        })
        .collect()
}

/// 削除ログ（監査ログ、D1 / Issue #51）の1行分の表示用データ。
pub struct AuditRow {
    /// 経過時間の相対表現（例: "3日前"）。
    pub when: String,
    /// ルール・サイズ・結果のまとめ。
    pub summary: String,
    /// 削除対象のパス。
    pub path: String,
}

/// `records` から、新しい順（`records` は追記順＝古い順に並んでいる前提）に
/// 最大 `limit` 件の表示行を作る。`history_rows` と同じく `now_secs` は
/// 呼び出し側が渡す（本モジュールを `egui` 非依存に保つため）。
pub fn audit_rows(
    records: &[AuditRecord],
    now_secs: u64,
    limit: usize,
    lang: Lang,
) -> Vec<AuditRow> {
    records
        .iter()
        .rev()
        .take(limit)
        .map(|record| {
            let seconds_ago = now_secs.saturating_sub(record.timestamp_secs);
            // `message`（`ItemOutcome::Failed`）は core 由来の文言のため翻訳しない
            // （OS エラー文言等をそのまま表示する。out of scope、E4 / Issue #58）。
            let outcome = match (&record.outcome, lang) {
                (ItemOutcome::Deleted, Lang::Ja) => "削除".to_string(),
                (ItemOutcome::Deleted, Lang::En) => "Deleted".to_string(),
                (ItemOutcome::Failed { message }, Lang::Ja) => format!("失敗（{message}）"),
                (ItemOutcome::Failed { message }, Lang::En) => format!("Failed ({message})"),
                (ItemOutcome::Missing, Lang::Ja) => "対象なし".to_string(),
                (ItemOutcome::Missing, Lang::En) => "Not found".to_string(),
                (ItemOutcome::NotAttempted, Lang::Ja) => "未実行".to_string(),
                (ItemOutcome::NotAttempted, Lang::En) => "Not attempted".to_string(),
            };
            AuditRow {
                when: relative_days(seconds_ago, lang),
                summary: format!("{} {} {}", record.rule_id, human_size(record.size), outcome),
                path: record.path.display().to_string(),
            }
        })
        .collect()
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
        assert_eq!(safety_label(Safety::Safe, Lang::Ja), "安全");
        assert_eq!(safety_label(Safety::Caution, Lang::Ja), "注意");
        assert_eq!(safety_label(Safety::Review, Lang::Ja), "要確認");
    }

    #[test]
    fn safety_label_en_differs_from_ja() {
        let en = safety_label(Safety::Safe, Lang::En);
        assert!(!en.is_empty());
        assert_ne!(en, safety_label(Safety::Safe, Lang::Ja));
    }

    #[test]
    fn age_label_formats_known_cases() {
        assert_eq!(age_label(Some(0), Lang::Ja), "本日");
        assert_eq!(age_label(Some(5), Lang::Ja), "5日");
        assert_eq!(age_label(None, Lang::Ja), "不明");
    }

    #[test]
    fn large_file_badge_is_none_without_a_threshold() {
        assert_eq!(large_file_badge(1_000_000_000, None, Lang::Ja), None);
    }

    #[test]
    fn large_file_badge_appears_at_or_above_threshold() {
        assert_eq!(large_file_badge(999, Some(1_000), Lang::Ja), None);
        assert!(large_file_badge(1_000, Some(1_000), Lang::Ja).is_some());
        assert!(large_file_badge(2_000, Some(1_000), Lang::Ja).is_some());
    }

    #[test]
    fn duplicate_badge_is_none_for_primary_or_missing() {
        use pc_cleaner_core::DuplicateInfo;
        assert_eq!(duplicate_badge(None, Lang::Ja), None);
        assert_eq!(
            duplicate_badge(
                Some(DuplicateInfo {
                    group_id: 0,
                    group_size: 3,
                    is_primary: true,
                }),
                Lang::Ja
            ),
            None
        );
    }

    #[test]
    fn duplicate_badge_shows_remaining_count_for_non_primary() {
        use pc_cleaner_core::DuplicateInfo;
        let badge = duplicate_badge(
            Some(DuplicateInfo {
                group_id: 0,
                group_size: 3,
                is_primary: false,
            }),
            Lang::Ja,
        )
        .unwrap();
        assert!(badge.contains('2'));
    }

    #[test]
    fn future_rules_contains_only_needs_admin_rules_when_not_elevated() {
        use pc_cleaner_core::rule::builtin_rules;

        let all = builtin_rules(&Config::default());
        let rules = future_rules(&all, false);
        assert!(!rules.is_empty());
        assert!(rules.iter().all(|r| r.needs_admin));
        assert!(rules.iter().any(|r| r.id == "system_temp"));
        assert!(rules.iter().any(|r| r.id == "windows_update_cache"));
        assert!(rules.iter().any(|r| r.id == "delivery_optimization_cache"));
    }

    #[test]
    fn future_rules_is_empty_when_elevated() {
        use pc_cleaner_core::rule::builtin_rules;

        let all = builtin_rules(&Config::default());
        assert!(future_rules(&all, true).is_empty());
    }

    #[test]
    fn future_rules_and_scannable_rules_are_exact_complements() {
        use pc_cleaner_core::rule::{builtin_rules, scannable_rules};

        let config = Config::default();
        let all = builtin_rules(&config);
        for elevated in [false, true] {
            let future = future_rules(&all, elevated);
            let scannable = scannable_rules(&config, elevated);
            assert_eq!(
                future.len() + scannable.len(),
                all.len(),
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
            assert!(!skip_reason_label(&reason, Lang::Ja).is_empty());
        }
    }

    #[test]
    fn exclusion_label_covers_all_variants_except_not_selected() {
        use pc_cleaner_core::ExclusionReason as R;
        assert_eq!(exclusion_label(R::NotSelected, Lang::Ja), None);
        for reason in [
            R::UnknownRule,
            R::NeedsAdmin,
            R::OutsideRuleBase,
            R::UnresolvedBase,
            R::RequiresPermanent,
        ] {
            assert!(exclusion_label(reason, Lang::Ja).is_some());
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
        assert!(!elevate_confirm_text(Lang::Ja).is_empty());
        assert!(!elevate_confirm_text(Lang::En).is_empty());
    }

    fn test_rule(id: &str, label: &str) -> Rule {
        Rule {
            id: id.to_string(),
            label: label.to_string(),
            description: String::new(),
            base: pc_cleaner_core::KnownDir::UserTemp,
            match_kind: pc_cleaner_core::MatchKind::All,
            needs_admin: false,
            safety: Safety::Safe,
            age_threshold_days: None,
            large_file_threshold_bytes: None,
            is_user_defined: false,
        }
    }

    fn history_entry(
        timestamp_secs: u64,
        freed_bytes: u64,
        deleted_count: usize,
    ) -> pc_cleaner_core::HistoryEntry {
        history_entry_with_run_id(0, timestamp_secs, freed_bytes, deleted_count)
    }

    fn history_entry_with_run_id(
        run_id: u64,
        timestamp_secs: u64,
        freed_bytes: u64,
        deleted_count: usize,
    ) -> pc_cleaner_core::HistoryEntry {
        pc_cleaner_core::HistoryEntry {
            run_id,
            timestamp_secs,
            freed_bytes,
            deleted_count,
            failed_count: 0,
            permanent: false,
        }
    }

    fn audit_record(run_id: u64, timestamp_secs: u64, path: &str) -> AuditRecord {
        AuditRecord {
            run_id,
            timestamp_secs,
            rule_id: "user_temp".to_string(),
            path: PathBuf::from(path),
            size: 1024,
            action: pc_cleaner_core::DeleteAction::ToTrash,
            outcome: ItemOutcome::Deleted,
            method: pc_cleaner_core::DeleteMethod::Trash,
        }
    }

    #[test]
    fn category_rows_resolves_labels_from_rules() {
        let breakdown = vec![CategoryBreakdown {
            rule_id: "user_temp".to_string(),
            item_count: 3,
            file_count: 10,
            total_size: 50,
        }];
        let mut rules = HashMap::new();
        rules.insert(
            "user_temp".to_string(),
            test_rule("user_temp", "ユーザー一時ファイル"),
        );

        let rows = category_rows(&breakdown, &rules, 100, Lang::Ja);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "ユーザー一時ファイル");
        assert_eq!(rows[0].detail, "3 件 / 50 B");
        assert!((rows[0].fraction - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn category_rows_falls_back_to_rule_id_when_rule_unknown() {
        let breakdown = vec![CategoryBreakdown {
            rule_id: "unknown_rule".to_string(),
            item_count: 1,
            file_count: 1,
            total_size: 10,
        }];
        let rules = HashMap::new();

        let rows = category_rows(&breakdown, &rules, 10, Lang::Ja);
        assert_eq!(rows[0].label, "unknown_rule");
    }

    #[test]
    fn rows_have_zero_fraction_when_total_is_zero() {
        let breakdown = vec![CategoryBreakdown {
            rule_id: "a".to_string(),
            item_count: 0,
            file_count: 0,
            total_size: 0,
        }];
        let rows = category_rows(&breakdown, &HashMap::new(), 0, Lang::Ja);
        assert_eq!(rows[0].fraction, 0.0);

        let buckets = vec![pc_cleaner_core::BucketBreakdown {
            bucket: pc_cleaner_core::SizeBucket::UnderMib,
            item_count: 1,
            total_size: 0,
        }];
        let bucket_rows = bucket_rows(&buckets, 0, Lang::Ja);
        assert_eq!(bucket_rows[0].fraction, 0.0);
    }

    #[test]
    fn bucket_rows_omit_empty_buckets() {
        let buckets = vec![
            pc_cleaner_core::BucketBreakdown {
                bucket: pc_cleaner_core::SizeBucket::UnderMib,
                item_count: 2,
                total_size: 20,
            },
            pc_cleaner_core::BucketBreakdown {
                bucket: pc_cleaner_core::SizeBucket::Mib1To10,
                item_count: 0,
                total_size: 0,
            },
        ];
        let rows = bucket_rows(&buckets, 20, Lang::Ja);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "1MB未満");
    }

    #[test]
    fn history_rows_are_newest_first_and_respect_the_limit() {
        let mut history = History::default();
        for i in 0..5u64 {
            history
                .entries
                .push(history_entry(i * 1000, 1024 * (i + 1), i as usize + 1));
        }
        let now_secs = 5000;
        let rows = history_rows(&history, now_secs, 2, Lang::Ja);
        assert_eq!(rows.len(), 2);
        // 最新（i=4, timestamp=4000）が先頭に来ること。
        assert!(rows[0].summary.contains("5.0 KB"));
        assert!(rows[0].summary.contains("5 件"));
        // 次点（i=3, timestamp=3000）。
        assert!(rows[1].summary.contains("4.0 KB"));
    }

    #[test]
    fn history_rows_carry_through_the_run_id() {
        let mut history = History::default();
        history
            .entries
            .push(history_entry_with_run_id(42, 100, 1024, 1));
        let rows = history_rows(&history, 200, 5, Lang::Ja);
        assert_eq!(rows[0].run_id, 42);
    }

    #[test]
    fn history_rows_handles_empty_history() {
        let history = History::default();
        assert!(history_rows(&history, 1000, 5, Lang::Ja).is_empty());
    }

    #[test]
    fn audit_rows_are_newest_first_and_respect_the_limit() {
        let records = vec![
            audit_record(1, 0, "a.tmp"),
            audit_record(1, 1000, "b.tmp"),
            audit_record(2, 2000, "c.tmp"),
        ];
        let rows = audit_rows(&records, 2000, 2, Lang::Ja);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].path.ends_with("c.tmp"), "最新が先頭に来ること");
        assert!(rows[1].path.ends_with("b.tmp"));
    }

    #[test]
    fn audit_rows_handles_empty_records() {
        assert!(audit_rows(&[], 1000, 5, Lang::Ja).is_empty());
    }

    #[test]
    fn audit_rows_summary_reflects_outcome() {
        let mut record = audit_record(1, 0, "a.tmp");
        record.outcome = ItemOutcome::Failed {
            message: "使用中".to_string(),
        };
        let rows = audit_rows(std::slice::from_ref(&record), 0, 5, Lang::Ja);
        assert!(rows[0].summary.contains("失敗"));
        assert!(rows[0].summary.contains("使用中"));
    }
}
