//! 掃除ルール（許可リストの1項目）。判定支援の一次情報。

use crate::platform::KnownDir;

/// ルールの安全度区分。既定の選択状態と表示方法を決定する（5.2.1）。
///
/// variant の宣言順はリスクの昇順（`Safe` が最も安全）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Safety {
    /// 再生成される一時領域・キャッシュ。既定でチェック ON。
    Safe,
    /// 状況次第（古いダウンロード、大きなログ等）。既定 OFF・注意色で表示。
    Caution,
    /// 中身の確認が必要。既定 OFF・確認前提。
    Review,
}

/// ルール内での対象絞り込み方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchKind {
    /// 基点配下の全件を対象とする。
    All,
    /// 指定拡張子のみを対象とする。
    ///
    /// 拡張子は先頭ドットなし・小文字で保持する（例: `"log"`, `"tmp"`）。
    Extension(Vec<String>),
    /// 走査時の絞り込み自体を経過日数で行う。`Rule::age_threshold_days` と
    /// 併用し、しきい値未満のものは走査結果に含めない。
    OlderThan,
}

/// 掃除ルール。許可リストの1項目であり、判定支援の一次情報。
///
/// 走査の基点・絞り込み条件・安全度・説明文などを保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// 安定した識別子（例: `"user_temp"`）。
    pub id: String,
    /// 表示名（例: 「ユーザー一時ファイル」）。
    pub label: String,
    /// このルールが何を・なぜ対象とするかの説明文（F-REC-01）。
    pub description: String,
    /// 走査の基点（抽象キー）。
    pub base: KnownDir,
    /// 対象の絞り込み方式。
    pub match_kind: MatchKind,
    /// 削除に管理者権限が要るか。`true` のルールは初版ではフィルタ除外する（F-SCAN-04）。
    pub needs_admin: bool,
    /// 安全度区分。
    pub safety: Safety,
    /// 経過日数のしきい値。
    ///
    /// `match_kind == MatchKind::OlderThan` のルールでは走査時の絞り込みに
    /// 使われる。それ以外の `match_kind` では [`crate::recommend::recommend`]
    /// 専用のしきい値であり、走査時の絞り込みには使わない。
    ///
    /// 初版ではルールにハードコードするが、将来 `Config` へ移設できるよう
    /// `Rule` 本体とは独立したフィールドとして分離しておく（NF-EXT-03 / 将来対応 C1）。
    pub age_threshold_days: Option<u64>,
}

/// `old_logs` ルールの推奨判定に用いる経過日数のしきい値（日）。
const OLD_LOG_THRESHOLD_DAYS: u64 = 180;

/// `old_downloads` ルールの走査時の絞り込みに用いる経過日数のしきい値（日）。
const OLD_DOWNLOAD_THRESHOLD_DAYS: u64 = 90;

/// 初版の組み込みルールセット（許可リスト全体、5.7）。
///
/// `needs_admin` なルール（`system_temp`）も含む。UI は一覧に「将来対応」と
/// 表示するため（NF-SAF-05）、走査・削除の対象だけに絞り込みたい場合は
/// [`scannable_rules`] を使う。
pub fn builtin_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "user_temp".to_string(),
            label: "ユーザー一時ファイル".to_string(),
            description: "アプリが処理の途中経過を書き出す一時ファイルの置き場です。\
                必要になれば自動的に作り直されるため、削除しても設定やデータは失われません。\
                本日更新されたものなど使用中の可能性があるものは、自動的に推奨から外します。"
                .to_string(),
            base: KnownDir::UserTemp,
            match_kind: MatchKind::All,
            needs_admin: false,
            safety: Safety::Safe,
            age_threshold_days: None,
        },
        Rule {
            id: "browser_cache".to_string(),
            label: "ブラウザ / アプリのキャッシュ".to_string(),
            description: "Web ページの画像やスクリプトなどを、次回アクセス時に再ダウンロード\
                せずに済ませるための一時保存領域です。削除してもブックマーク・保存済み\
                パスワード・閲覧履歴・ログイン状態は消えません。次にアクセスしたときに\
                自動的に作り直され、その分だけ表示が少し遅くなります。"
                .to_string(),
            base: KnownDir::Cache,
            match_kind: MatchKind::All,
            needs_admin: false,
            safety: Safety::Safe,
            age_threshold_days: None,
        },
        Rule {
            id: "thumbnail_cache".to_string(),
            label: "サムネイルキャッシュ".to_string(),
            description: "エクスプローラーでフォルダを開いたときに、画像や動画の縮小版を\
                すばやく表示するためのキャッシュです。削除しても元の画像・動画そのものは\
                消えません。次に同じフォルダを開いたときに自動的に作り直されます。"
                .to_string(),
            base: KnownDir::LocalAppData,
            match_kind: MatchKind::Extension(vec!["db".to_string()]),
            needs_admin: false,
            safety: Safety::Safe,
            age_threshold_days: None,
        },
        Rule {
            id: "recycle_bin".to_string(),
            label: "ゴミ箱".to_string(),
            description: "すでにゴミ箱へ移動済みのファイルです。ここで削除すると完全に消え、\
                復元できなくなります。見覚えのないファイルが含まれていないか、実行前に\
                一度ゴミ箱の中身を確認してください。"
                .to_string(),
            base: KnownDir::RecycleBin,
            match_kind: MatchKind::All,
            needs_admin: false,
            // 要件定義書 5.7 は Safe（既定 ON）と定めるが、ゴミ箱の中身を消す
            // 操作にはゴミ箱という退避先が無く、ワンクリック掃除（フロー①）が
            // 確認1回でゴミ箱を完全に空にしてしまう。設計目標 G2「ゴミ箱経由・
            // 復旧可能」との衝突を避けるため Caution（既定 OFF）に格下げする
            // （docs/requirements.md 5.7 も合わせて更新済み）。
            safety: Safety::Caution,
            age_threshold_days: None,
        },
        Rule {
            id: "old_logs".to_string(),
            label: format!("古いログ（{OLD_LOG_THRESHOLD_DAYS}日超）"),
            description: format!(
                "アプリが動作記録として書き出したログファイルです。最終更新から\
                {OLD_LOG_THRESHOLD_DAYS}日以上経過したものを推奨対象としています。\
                過去の不具合を調査する予定がなければ削除して問題ありません。\
                現在調査中の不具合がある場合は残してください。"
            ),
            base: KnownDir::LocalAppData,
            match_kind: MatchKind::Extension(vec!["log".to_string()]),
            needs_admin: false,
            safety: Safety::Caution,
            age_threshold_days: Some(OLD_LOG_THRESHOLD_DAYS),
        },
        Rule {
            id: "old_downloads".to_string(),
            label: format!("古いダウンロード（{OLD_DOWNLOAD_THRESHOLD_DAYS}日超）"),
            description: format!(
                "ダウンロードフォルダにあり、最終更新から{OLD_DOWNLOAD_THRESHOLD_DAYS}日以上\
                経過したファイルです。インストーラや資料など、本人にしか要否を判断できない\
                ものが含まれます。既定では選択しません。削除する前に必ず中身を確認してください。"
            ),
            base: KnownDir::Downloads,
            match_kind: MatchKind::OlderThan,
            needs_admin: false,
            safety: Safety::Review,
            age_threshold_days: Some(OLD_DOWNLOAD_THRESHOLD_DAYS),
        },
        Rule {
            id: "system_temp".to_string(),
            label: "システム一時ファイル（将来対応）".to_string(),
            description: "OS とシステムサービスが使う一時ファイルの置き場です。削除には\
                管理者権限が必要なため、本バージョンでは走査・削除の対象外です（将来対応）。"
                .to_string(),
            base: KnownDir::SystemTemp,
            match_kind: MatchKind::All,
            needs_admin: true,
            // 実体は再生成される一時領域で意味的には Safe だが、将来 A1 で
            // needs_admin を false に切り替えた瞬間に既定 ON となり管理者領域を
            // 無確認で削除するリスクがある。recommend() は経過日数によらず
            // Review を自動 ON にしないため、Review にしておくことで二重に
            // 防御する（NF-SAF-01）。
            safety: Safety::Review,
            age_threshold_days: None,
        },
    ]
}

/// 走査・削除の対象となるルールのみを返す（`needs_admin == false`）。
/// #5（scan）が使う（F-SCAN-04）。
pub fn scannable_rules() -> Vec<Rule> {
    builtin_rules()
        .into_iter()
        .filter(|r| !r.needs_admin)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn safety_ordered_by_risk_ascending() {
        assert!(Safety::Safe < Safety::Caution);
        assert!(Safety::Caution < Safety::Review);
    }

    #[test]
    fn builtin_rules_have_stable_unique_ids() {
        let rules = builtin_rules();
        let ids: HashSet<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        let expected: HashSet<&str> = [
            "user_temp",
            "browser_cache",
            "thumbnail_cache",
            "recycle_bin",
            "old_logs",
            "old_downloads",
            "system_temp",
        ]
        .into_iter()
        .collect();
        assert_eq!(ids, expected);
        assert_eq!(rules.len(), expected.len(), "id が重複していない");
    }

    #[test]
    fn every_rule_has_non_empty_id_label_description() {
        for rule in builtin_rules() {
            assert!(!rule.id.is_empty());
            assert!(!rule.label.is_empty());
            assert!(!rule.description.is_empty());
        }
    }

    #[test]
    fn builtin_rules_cover_all_safety_levels() {
        let rules = builtin_rules();
        assert!(rules.iter().any(|r| r.safety == Safety::Safe));
        assert!(rules.iter().any(|r| r.safety == Safety::Caution));
        assert!(rules.iter().any(|r| r.safety == Safety::Review));
    }

    #[test]
    fn only_system_temp_needs_admin_and_is_excluded_from_scan() {
        let rules = builtin_rules();
        let admin_ids: Vec<&str> = rules
            .iter()
            .filter(|r| r.needs_admin)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(admin_ids, vec!["system_temp"]);

        let scannable = scannable_rules();
        assert_eq!(scannable.len(), rules.len() - 1);
        assert!(scannable.iter().all(|r| r.id != "system_temp"));
    }

    #[test]
    fn age_thresholds_match_the_specification() {
        for rule in builtin_rules() {
            let expected = match rule.id.as_str() {
                "old_logs" => Some(180),
                "old_downloads" => Some(90),
                _ => None,
            };
            assert_eq!(
                rule.age_threshold_days, expected,
                "id={} の age_threshold_days",
                rule.id
            );
        }
    }

    #[test]
    fn rule_text_does_not_contain_a_raw_windows_path() {
        for rule in builtin_rules() {
            assert!(!rule.label.contains(':'), "id={}", rule.id);
            assert!(!rule.label.contains('\\'), "id={}", rule.id);
            assert!(!rule.description.contains(':'), "id={}", rule.id);
            assert!(!rule.description.contains('\\'), "id={}", rule.id);
        }
    }
}
