//! 判定支援ロジック（推奨の動的補正）。F-REC-04〜08。

use crate::entry::ScanEntry;
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

/// `entry.age_days` が `Some(0)`（最終更新が本日）であることを
/// 「使用中の可能性がある」とみなす近似。
///
/// 純粋関数制約（NF-MNT-02）により現在時刻を参照できないため、`entry.modified`
/// ではなく走査時（#5）に算出済みの `age_days` のみで判定する。日単位の粗い
/// 近似であり、ファイルハンドル参照等による精緻化は将来対応 C2 でスコープ外。
fn is_possibly_in_use(entry: &ScanEntry) -> bool {
    entry.age_days == Some(0)
}

/// 経過日数を説明する文言を生成する。`entry.modified` は参照しない
/// （F-REC-05 / NF-MNT-02、`recommend` の純粋性を保つため）。
fn describe_age(age_days: Option<u64>) -> String {
    match age_days {
        Some(0) => "本日更新されています。".to_string(),
        Some(n) => format!("最終更新から{n}日経過しています。"),
        None => "最終更新日時は不明です。".to_string(),
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
pub fn recommend(entry: &ScanEntry, rule: &Rule) -> Recommendation {
    let age_note = describe_age(entry.age_days);

    if rule.needs_admin {
        // NF-SAF-05 / A1 / Issue #40。管理者権限領域は、昇格済みであっても
        // 自動では推奨しない。`Safety::Review` と合わせた二重防御であり、
        // ユーザーが明示的にチェックを入れた場合のみ対象になる。`recommend`
        // は純粋関数の契約（F-REC-05）を守るため `Platform` を受け取らず、
        // 昇格状態そのものは参照しない（#5 / #7 のフィルタ漏れに対する
        // 最後の防御としての役割は変わらない）。
        return Recommendation::new(
            false,
            "管理者権限が必要な領域です。内容を確認したうえで、必要な場合のみ手動で選択してください。",
        );
    }

    match rule.safety {
        Safety::Safe => {
            if is_possibly_in_use(entry) {
                Recommendation::new(
                    false,
                    format!("{age_note}使用中の可能性があるため推奨から外しました。"),
                )
            } else {
                Recommendation::new(
                    true,
                    format!(
                        "再生成される一時領域のため、削除しても自動的に作り直されます。{age_note}"
                    ),
                )
            }
        }
        Safety::Caution => {
            if is_possibly_in_use(entry) {
                Recommendation::new(
                    false,
                    format!("{age_note}使用中の可能性があるため推奨から外しました。"),
                )
            } else {
                match (entry.age_days, rule.age_threshold_days) {
                    (Some(age), Some(threshold)) if age >= threshold => Recommendation::new(
                        true,
                        format!(
                            "{age_note}しきい値（{threshold}日）を超えて更新されていないため推奨します。"
                        ),
                    ),
                    (Some(_), Some(threshold)) => Recommendation::new(
                        false,
                        format!(
                            "{age_note}しきい値（{threshold}日）未満のため、内容を確認してから選択してください。"
                        ),
                    ),
                    _ => Recommendation::new(
                        false,
                        format!("{age_note}内容を確認してから選択してください。"),
                    ),
                }
            }
        }
        Safety::Review => Recommendation::new(
            false,
            format!("{age_note}中身の確認が必要な領域です。選択する前に内容を確認してください。"),
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
            recommended: false,
            reason: String::new(),
            selected: false,
        }
    }

    #[test]
    fn safe_temp_updated_today_is_not_recommended() {
        let r = rule(Safety::Safe, None, false);
        let e = entry(Some(0), None);
        let rec = recommend(&e, &r);
        assert!(!rec.recommended);
        assert!(rec.reason.contains("使用中"));
    }

    #[test]
    fn caution_log_older_than_threshold_is_recommended() {
        let r = rule(Safety::Caution, Some(180), false);
        let e = entry(Some(200), None);
        let rec = recommend(&e, &r);
        assert!(rec.recommended);
    }

    #[test]
    fn safety_determines_default_recommendation() {
        let e = entry(Some(30), None);

        let safe = rule(Safety::Safe, None, false);
        assert!(recommend(&e, &safe).recommended);

        let caution = rule(Safety::Caution, Some(180), false);
        assert!(!recommend(&e, &caution).recommended);

        let review = rule(Safety::Review, Some(90), false);
        assert!(!recommend(&e, &review).recommended);
    }

    #[test]
    fn review_is_never_auto_recommended_however_old() {
        let r = rule(Safety::Review, Some(90), false);
        let e = entry(Some(9999), None);
        assert!(!recommend(&e, &r).recommended);
    }

    #[test]
    fn needs_admin_rule_is_never_recommended() {
        for safety in [Safety::Safe, Safety::Caution, Safety::Review] {
            let r = rule(safety, Some(1), true);
            let e = entry(Some(9999), None);
            let rec = recommend(&e, &r);
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
                    assert!(!recommend(&e, &r).reason.is_empty());
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

        assert_eq!(recommend(&base, &r), recommend(&with_epoch, &r));
        assert_eq!(recommend(&base, &r), recommend(&with_now, &r));
    }
}
