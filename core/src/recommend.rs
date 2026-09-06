//! 判定支援ロジック（推奨の動的補正）。F-REC-04〜08。

use crate::entry::ScanEntry;
use crate::format::human_size;
use crate::i18n::Lang;
use crate::rule::{Rule, Safety};

/// [`recommend`] の戻り値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    /// 推奨可否。
    pub recommended: bool,
    /// 推奨・非推奨の理由（短文）。
    pub reason: String,
}

impl Recommendation {
    fn new(recommended: bool, reason: impl Into<String>) -> Self {
        Recommendation {
            recommended,
            reason: reason.into(),
        }
    }
}

/// エントリが「使用中の可能性がある」かどうかを判定する（C2 / Issue #48）。
///
/// 優先順位は次のとおり：
/// 1. `entry.in_use`（走査時に `inspect::annotate_in_use` が算出済みの
///    ファイルハンドル判定）が `Some(true)` なら、それを信頼して使用中と
///    みなす。
/// 2. `entry.in_use` が `Some(false)`（明確に空いていると判定できた）なら、
///    `age_days` に関わらず使用中とはみなさない。
/// 3. `entry.in_use` が判定不能（`None`。ディレクトリ集約エントリ・権限
///    不足等）だった場合にのみ、`entry.age_days == Some(0)`（最終更新が
///    本日）という粗い近似にフォールバックする。
///
/// 純粋関数制約（NF-MNT-02）により現在時刻・ファイル I/O を本関数から直接
/// 参照することはできないため、いずれも走査時（#5 / `inspect`）に算出済みの
/// 値のみで判定する。
fn is_possibly_in_use(entry: &ScanEntry) -> bool {
    entry.in_use == Some(true) || (entry.in_use.is_none() && entry.age_days == Some(0))
}

/// 経過日数を説明する文言を生成する。`entry.modified` は参照しない
/// （F-REC-05 / NF-MNT-02、`recommend` の純粋性を保つため）。
fn describe_age(age_days: Option<u64>, lang: Lang) -> String {
    match (age_days, lang) {
        (Some(0), Lang::Ja) => "本日更新されています。".to_string(),
        (Some(0), Lang::En) => "Updated today.".to_string(),
        (Some(n), Lang::Ja) => format!("最終更新から{n}日経過しています。"),
        (Some(n), Lang::En) => format!("Last modified {n} days ago."),
        (None, Lang::Ja) => "最終更新日時は不明です。".to_string(),
        (None, Lang::En) => "Last modified time is unknown.".to_string(),
    }
}

/// `entry` と `rule` を入力に、状況に応じた推奨可否と理由を返す純粋関数。
///
/// 契約: I/O を行わない・現在時刻を参照しない（`age_days` は呼び出し側が
/// 走査時に算出済みの値を渡す）。これにより単体テスト可能かつ CLI/GUI で
/// 同一に作用することを保証する（F-REC-05）。
///
/// 判定は「安全度 × 経過日数 × 使用中の可能性」の組み合わせで行う
/// （F-REC-07）。`rule.description` / `rule.safety` と本関数が返す `reason` /
/// `entry.age_days` を合わせて提示することで、ユーザーは個別ファイルを開かず
/// に判断できる（F-REC-01 / F-REC-08）。`Config` / `RulePref` によるユーザー
/// 既定の上書きは本関数の責務ではなく #5 / #8 が行う。
///
/// 大容量ファイル・重複ファイルの注意書き（C2 / Issue #48）は推奨可否
/// そのものには影響しない付加情報のため、[`base_recommendation`] が決めた
/// 推奨可否はそのまま維持し、`reason` にだけ追記する。
pub fn recommend(entry: &ScanEntry, rule: &Rule, lang: Lang) -> Recommendation {
    let mut recommendation = base_recommendation(entry, rule, lang);

    if let Some(threshold) = rule.large_file_threshold_bytes {
        if entry.size >= threshold {
            recommendation.reason.push_str(&match lang {
                Lang::Ja => format!(
                    "サイズが大きい（{}）ため、削除前に内容を確認することをおすすめします。",
                    human_size(entry.size)
                ),
                Lang::En => format!(
                    "This is large ({}), so we recommend checking the contents before \
                    deleting.",
                    human_size(entry.size)
                ),
            });
        }
    }

    if let Some(info) = entry.duplicate {
        if !info.is_primary {
            recommendation.reason.push_str(&match lang {
                Lang::Ja => format!(
                    "同一内容のファイルが他に{}件あります。",
                    info.group_size - 1
                ),
                Lang::En => format!(
                    "There are {} other files with identical content.",
                    info.group_size - 1
                ),
            });
        }
    }

    recommendation
}

/// `rule.needs_admin` / `rule.safety` / 経過日数・使用中判定に基づく基本の
/// 推奨可否と理由を決める（大容量・重複ファイルの注意書きを含まない）。
fn base_recommendation(entry: &ScanEntry, rule: &Rule, lang: Lang) -> Recommendation {
    let age_note = describe_age(entry.age_days, lang);

    if rule.needs_admin {
        // NF-SAF-05 / A1 / Issue #40。管理者権限領域は、昇格済みであっても
        // 自動では推奨しない。`Safety::Review` と合わせた二重防御であり、
        // ユーザーが明示的にチェックを入れた場合のみ対象になる。`recommend`
        // は純粋関数の契約（F-REC-05）を守るため `Platform` を受け取らず、
        // 昇格状態そのものは参照しない（#5 / #7 のフィルタ漏れに対する
        // 最後の防御としての役割は変わらない）。
        return Recommendation::new(
            false,
            match lang {
                Lang::Ja => {
                    "管理者権限が必要な領域です。内容を確認したうえで、必要な場合のみ手動で選択してください。"
                }
                Lang::En => {
                    "This area requires administrator privileges. Please check the \
                    contents and select it manually only if needed."
                }
            },
        );
    }

    match rule.safety {
        Safety::Safe => {
            if is_possibly_in_use(entry) {
                Recommendation::new(
                    false,
                    match lang {
                        Lang::Ja => {
                            format!("{age_note}使用中の可能性があるため推奨から外しました。")
                        }
                        Lang::En => format!(
                            "{age_note} Excluded from the recommendation because it may be \
                            in use."
                        ),
                    },
                )
            } else {
                Recommendation::new(
                    true,
                    match lang {
                        Lang::Ja => format!(
                            "再生成される一時領域のため、削除しても自動的に作り直されます。{age_note}"
                        ),
                        Lang::En => format!(
                            "This is a temporary area that is recreated automatically, so \
                            deleting it is safe. {age_note}"
                        ),
                    },
                )
            }
        }
        Safety::Caution => {
            if is_possibly_in_use(entry) {
                Recommendation::new(
                    false,
                    match lang {
                        Lang::Ja => {
                            format!("{age_note}使用中の可能性があるため推奨から外しました。")
                        }
                        Lang::En => format!(
                            "{age_note} Excluded from the recommendation because it may be \
                            in use."
                        ),
                    },
                )
            } else {
                match (entry.age_days, rule.age_threshold_days) {
                    (Some(age), Some(threshold)) if age >= threshold => Recommendation::new(
                        true,
                        match lang {
                            Lang::Ja => format!(
                                "{age_note}しきい値（{threshold}日）を超えて更新されていないため推奨します。"
                            ),
                            Lang::En => format!(
                                "{age_note} Recommended because it has not been modified \
                                within the threshold ({threshold} days)."
                            ),
                        },
                    ),
                    (Some(_), Some(threshold)) => Recommendation::new(
                        false,
                        match lang {
                            Lang::Ja => format!(
                                "{age_note}しきい値（{threshold}日）未満のため、内容を確認してから選択してください。"
                            ),
                            Lang::En => format!(
                                "{age_note} This is within the threshold ({threshold} days), \
                                so please check the contents before selecting it."
                            ),
                        },
                    ),
                    _ => Recommendation::new(
                        false,
                        match lang {
                            Lang::Ja => format!("{age_note}内容を確認してから選択してください。"),
                            Lang::En => {
                                format!("{age_note} Please check the contents before selecting it.")
                            }
                        },
                    ),
                }
            }
        }
        Safety::Review => Recommendation::new(
            false,
            match lang {
                Lang::Ja => format!(
                    "{age_note}中身の確認が必要な領域です。選択する前に内容を確認してください。"
                ),
                Lang::En => format!(
                    "{age_note} This area requires reviewing the contents. Please check \
                    them before selecting it."
                ),
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::KnownDir;
    use crate::rule::MatchKind;
    use std::time::SystemTime;

    fn rule(safety: Safety, age_threshold_days: Option<u64>, needs_admin: bool) -> Rule {
        Rule {
            id: "test_rule".to_string(),
            label: "テストルール".to_string(),
            description: "テスト用".to_string(),
            base: KnownDir::UserTemp,
            match_kind: MatchKind::All,
            needs_admin,
            safety,
            age_threshold_days,
            large_file_threshold_bytes: None,
            is_user_defined: false,
        }
    }

    fn entry(age_days: Option<u64>, modified: Option<SystemTime>) -> ScanEntry {
        ScanEntry {
            rule_id: "test_rule".to_string(),
            path: "/tmp/x".into(),
            size: 0,
            file_count: 1,
            modified,
            age_days,
            in_use: None,
            duplicate: None,
            recommended: false,
            reason: String::new(),
            selected: false,
        }
    }

    #[test]
    fn safe_temp_updated_today_is_not_recommended() {
        let r = rule(Safety::Safe, None, false);
        let e = entry(Some(0), None);
        let rec = recommend(&e, &r, Lang::Ja);
        assert!(!rec.recommended);
        assert!(rec.reason.contains("使用中"));
    }

    #[test]
    fn caution_log_older_than_threshold_is_recommended() {
        let r = rule(Safety::Caution, Some(180), false);
        let e = entry(Some(200), None);
        let rec = recommend(&e, &r, Lang::Ja);
        assert!(rec.recommended);
    }

    #[test]
    fn safety_determines_default_recommendation() {
        let e = entry(Some(30), None);

        let safe = rule(Safety::Safe, None, false);
        assert!(recommend(&e, &safe, Lang::Ja).recommended);

        let caution = rule(Safety::Caution, Some(180), false);
        assert!(!recommend(&e, &caution, Lang::Ja).recommended);

        let review = rule(Safety::Review, Some(90), false);
        assert!(!recommend(&e, &review, Lang::Ja).recommended);
    }

    #[test]
    fn review_is_never_auto_recommended_however_old() {
        let r = rule(Safety::Review, Some(90), false);
        let e = entry(Some(9999), None);
        assert!(!recommend(&e, &r, Lang::Ja).recommended);
    }

    #[test]
    fn needs_admin_rule_is_never_recommended() {
        for safety in [Safety::Safe, Safety::Caution, Safety::Review] {
            let r = rule(safety, Some(1), true);
            let e = entry(Some(9999), None);
            let rec = recommend(&e, &r, Lang::Ja);
            assert!(!rec.recommended);
            assert!(rec.reason.contains("管理者権限"));
            assert!(
                !rec.reason.contains("将来対応"),
                "A1（Issue #40）で対応済みのため「将来対応」の文言は残さない"
            );
        }
    }

    #[test]
    fn reason_is_never_empty() {
        let ages = [
            None,
            Some(0),
            Some(1),
            Some(89),
            Some(90),
            Some(180),
            Some(9999),
        ];
        for safety in [Safety::Safe, Safety::Caution, Safety::Review] {
            for needs_admin in [false, true] {
                for age_days in ages {
                    let r = rule(safety, Some(90), needs_admin);
                    let e = entry(age_days, None);
                    assert!(!recommend(&e, &r, Lang::Ja).reason.is_empty());
                }
            }
        }
    }

    #[test]
    fn result_does_not_depend_on_modified_field() {
        let r = rule(Safety::Caution, Some(180), false);
        let base = entry(Some(200), None);
        let with_epoch = entry(Some(200), Some(SystemTime::UNIX_EPOCH));
        let with_now = entry(Some(200), Some(SystemTime::now()));

        assert_eq!(
            recommend(&base, &r, Lang::Ja),
            recommend(&with_epoch, &r, Lang::Ja)
        );
        assert_eq!(
            recommend(&base, &r, Lang::Ja),
            recommend(&with_now, &r, Lang::Ja)
        );
    }

    // ---- C2 / Issue #48: 使用中判定（entry.in_use）----

    #[test]
    fn in_use_flag_overrides_the_age_heuristic() {
        // 経過日数だけ見れば十分古い（Safe なら推奨されるはず）だが、
        // in_use = Some(true) が優先され推奨から外れること。
        let r = rule(Safety::Safe, None, false);
        let mut e = entry(Some(100), None);
        e.in_use = Some(true);

        let rec = recommend(&e, &r, Lang::Ja);
        assert!(!rec.recommended);
        assert!(rec.reason.contains("使用中"));
    }

    #[test]
    fn unknown_in_use_falls_back_to_age_zero_heuristic() {
        // in_use が判定不能（None）のときだけ、従来どおり age_days == 0 の
        // 近似にフォールバックする。
        let r = rule(Safety::Safe, None, false);
        let mut e = entry(Some(0), None);
        e.in_use = None;

        let rec = recommend(&e, &r, Lang::Ja);
        assert!(!rec.recommended, "in_use 不明のときは age_days==0 に倒す");
        assert!(rec.reason.contains("使用中"));
    }

    #[test]
    fn probe_says_free_beats_age_zero() {
        // in_use = Some(false)（明確に空いている）と分かっていれば、
        // age_days == 0（本日更新）であっても使用中扱いにしない。
        let r = rule(Safety::Safe, None, false);
        let mut e = entry(Some(0), None);
        e.in_use = Some(false);

        let rec = recommend(&e, &r, Lang::Ja);
        assert!(rec.recommended);
        assert!(!rec.reason.contains("使用中"));
    }

    // ---- C2 / Issue #48: 大容量ファイルの注意書き ----

    #[test]
    fn large_file_adds_a_note_without_changing_recommendation() {
        let mut r = rule(Safety::Safe, None, false);
        r.large_file_threshold_bytes = Some(1_000);

        let mut small = entry(Some(30), None);
        small.size = 999;
        let small_rec = recommend(&small, &r, Lang::Ja);
        assert!(small_rec.recommended);
        assert!(!small_rec.reason.contains("サイズが大きい"));

        let mut large = entry(Some(30), None);
        large.size = 1_000;
        let large_rec = recommend(&large, &r, Lang::Ja);
        assert!(
            large_rec.recommended,
            "サイズ注意書きは推奨可否を変えない（情報提供のみ）"
        );
        assert!(large_rec.reason.contains("サイズが大きい"));
    }

    // ---- C2 / Issue #48: 重複ファイルの注意書き ----

    #[test]
    fn duplicate_note_appears_for_non_primary_entries() {
        use crate::entry::DuplicateInfo;

        let r = rule(Safety::Safe, None, false);

        let mut primary = entry(Some(30), None);
        primary.duplicate = Some(DuplicateInfo {
            group_id: 0,
            group_size: 3,
            is_primary: true,
        });
        let primary_rec = recommend(&primary, &r, Lang::Ja);
        assert!(!primary_rec.reason.contains("同一内容のファイルが他に"));

        let mut secondary = entry(Some(30), None);
        secondary.duplicate = Some(DuplicateInfo {
            group_id: 0,
            group_size: 3,
            is_primary: false,
        });
        let secondary_rec = recommend(&secondary, &r, Lang::Ja);
        assert!(secondary_rec.recommended, "重複は推奨可否を変えない");
        assert!(
            secondary_rec
                .reason
                .contains("同一内容のファイルが他に2件あります")
        );
    }

    #[test]
    fn en_lang_produces_a_non_empty_ascii_reason() {
        // F-I18N-01 / Issue #58: 英語モードでも reason は空にならず、日本語
        // 混じりにならないことの簡単なスモークテスト。
        let r = rule(Safety::Caution, Some(180), false);
        let e = entry(Some(200), None);
        let rec = recommend(&e, &r, Lang::En);
        assert!(rec.recommended);
        assert!(!rec.reason.is_empty());
        assert!(rec.reason.is_ascii());
    }
}
