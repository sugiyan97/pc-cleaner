//! 掃除ルール（許可リストの1項目）。判定支援の一次情報。

use crate::config::Config;
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
    /// 「サイズが大きい」と見なすしきい値（バイト単位、C2 / Issue #48）。
    ///
    /// `Config::large_file_threshold_bytes` から `builtin_rules` が設定する。
    /// `recommend()` はこの値以上のエントリに注意書きを付け加えるだけで、
    /// 推奨可否そのものは変えない。大きな注意書きを出す意味が薄いルール
    /// （既に個別ファイル単位で確認が前提のもの等）では `None` のままでよい。
    pub large_file_threshold_bytes: Option<u64>,
}

impl Rule {
    /// このルールが、昇格状態 `elevated` のもとで走査・削除の対象になりうる
    /// か（F-SCAN-04 / A1 / Issue #40）。
    ///
    /// `scannable_rules` / `resolve_target`（scan.rs）/ `plan_entry`
    /// （delete.rs）/ `future_rules`（GUI）は、いずれもこのメソッドを経由する
    /// ことで判定を1箇所に集約する。
    pub fn is_permitted(&self, elevated: bool) -> bool {
        !self.needs_admin || elevated
    }

    /// 対象パスが管理者権限領域だったときに、それを許容してよいか。
    ///
    /// 管理者権限を要すると宣言したルール（`needs_admin == true`）が、
    /// 実際に昇格済みのときだけ `true` になる。昇格していても
    /// `needs_admin == false` のルールについては許容しない。これは、環境変数
    /// の設定ミス等で一般ルールの基点が意図せず管理者領域へ解決されて
    /// しまった場合の安全網（scan.rs / delete.rs の `Platform::requires_admin`
    /// 二次防御）を、昇格そのものによって無効化しないためである。昇格は
    /// 「ユーザーが選んだ管理者ルールへの同意」であって、全ルールへの
    /// 白紙委任ではない（NF-SAF-01 / Issue #40）。
    pub fn may_touch_admin_area(&self, elevated: bool) -> bool {
        self.needs_admin && elevated
    }
}

/// `old_logs` ルールの推奨判定に用いる経過日数のしきい値（日）。
const OLD_LOG_THRESHOLD_DAYS: u64 = 180;

/// `old_downloads` ルールの走査時の絞り込みに用いる経過日数のしきい値（日）。
const OLD_DOWNLOAD_THRESHOLD_DAYS: u64 = 90;

/// 初版の組み込みルールセット（許可リスト全体、5.7）。
///
/// `needs_admin` なルール（`system_temp`）も含む。UI は未昇格時、これらを
/// 一覧に「管理者権限が必要」として別枠表示するため（NF-SAF-05）、昇格状態を
/// 踏まえて走査・削除の対象だけに絞り込みたい場合は [`scannable_rules`] を
/// 使う。`config` は #48（C2）で追加された大容量ファイルしきい値
/// （`Config::large_file_threshold_bytes`）をルールへ持ち込むために必要になった。
pub fn builtin_rules(config: &Config) -> Vec<Rule> {
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
            large_file_threshold_bytes: Some(config.large_file_threshold_bytes),
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
            large_file_threshold_bytes: Some(config.large_file_threshold_bytes),
        },
        Rule {
            id: "thumbnail_cache".to_string(),
            label: "サムネイルキャッシュ".to_string(),
            description: "エクスプローラーでフォルダを開いたときに、画像や動画の縮小版を\
                すばやく表示するためのキャッシュです。削除しても元の画像・動画そのものは\
                消えません。次に同じフォルダを開いたときに自動的に作り直されます。"
                .to_string(),
            // 実体は %LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db
            // のみ。LocalAppData 全体を基点にすると無関係な .db ファイルを
            // 巻き込むため、専用の基点 ThumbnailCache に絞っている（Issue #16）。
            base: KnownDir::ThumbnailCache,
            match_kind: MatchKind::Extension(vec!["db".to_string()]),
            needs_admin: false,
            safety: Safety::Safe,
            age_threshold_days: None,
            // サムネイルキャッシュの個々のファイルが大容量になることは
            // ほぼ無く、大容量注意の告知に意味がないため None のまま。
            large_file_threshold_bytes: None,
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
            // 既にユーザーが「実行前に中身を確認」する前提のルールであり、
            // 大容量注意の告知を重ねる意味が薄いため None のまま。
            large_file_threshold_bytes: None,
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
            large_file_threshold_bytes: Some(config.large_file_threshold_bytes),
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
            large_file_threshold_bytes: Some(config.large_file_threshold_bytes),
        },
        Rule {
            id: "system_temp".to_string(),
            label: "システム一時ファイル".to_string(),
            description: "OS とシステムサービスが使う一時ファイルの置き場です。削除には\
                管理者権限が必要なため、管理者として実行し直した場合のみ対象になります。\
                使用中のファイルは削除できず、失敗として報告されます。"
                .to_string(),
            base: KnownDir::SystemTemp,
            match_kind: MatchKind::All,
            needs_admin: true,
            // 実体は再生成される一時領域で意味的には Safe だが、管理者権限
            // 領域を既定 ON にすると、昇格したユーザーが確認なしにシステム
            // 領域を消せてしまう。A1（Issue #40）では needs_admin を false に
            // 倒すのではなく「昇格済みなら通す」形にしたため（Rule::is_permitted
            // 参照）、Safety はここでも Review のまま据え置く。recommend() も
            // needs_admin ルールを推奨しないため二重に防御される（NF-SAF-01）。
            // この Safety を Safe や Caution に上げてはならない。
            safety: Safety::Review,
            age_threshold_days: None,
            // 管理者権限領域で、既に個別確認が前提のルールのため None のまま。
            large_file_threshold_bytes: None,
        },
        Rule {
            id: "windows_update_cache".to_string(),
            label: "Windows Update のダウンロード済みファイル".to_string(),
            description: "Windows Update が更新プログラムを適用するためにダウンロードした\
                ファイルの置き場です。適用済みの更新については残しておく必要がなく、\
                必要になれば自動的に再ダウンロードされます。削除には管理者権限が必要です。\
                更新の適用中やダウンロード中のファイルは使用中のため削除できず、\
                失敗として報告されます。その場合は再起動後にもう一度お試しください。"
                .to_string(),
            // 基点は SoftwareDistribution 全体ではなく Download サブフォルダに
            // 限定する。同階層の DataStore は更新履歴データベースであり、
            // 削除すると更新履歴が壊れるため対象にしてはならない（A3 /
            // Issue #42。KnownDir::WindowsUpdateCache の doc コメントも参照）。
            //
            // 本ルールの対応範囲はファイルの削除のみである。wuauserv
            // （Windows Update サービス）の停止は行わない。サービス制御は
            // このツールの責務を大きく超え、誤って行うと回復が難しいため、
            // 意図的にスコープ外としている。
            base: KnownDir::WindowsUpdateCache,
            match_kind: MatchKind::All,
            needs_admin: true,
            // system_temp と同じ理由で Review（既定 OFF・自動推奨なし）。
            // 解放できる容量が大きいことは「既定 ON にしてよい理由」には
            // ならない（NF-SAF-01）。この Safety を Safe や Caution に
            // 上げてはならない。
            safety: Safety::Review,
            age_threshold_days: None,
            // system_temp と同じ理由で None のまま。
            large_file_threshold_bytes: None,
        },
        Rule {
            id: "delivery_optimization_cache".to_string(),
            label: "配信の最適化ファイル".to_string(),
            description: "Windows Update やストアアプリの更新を、同じネットワーク上の\
                他の PC と共有するために保存されているファイルです。削除しても、\
                既に適用済みの更新には影響しません。必要になれば自動的に作り直されます。\
                削除には管理者権限が必要です。"
                .to_string(),
            // 実パスは NetworkService サービスアカウントのローカルプロファイル
            // 配下にある（KnownDir::DeliveryOptimizationCache の doc コメント
            // 参照）。グループポリシー DOModifyCacheDrive でキャッシュの保存先を
            // 変更している環境では、この固定パスは実体と一致せず走査結果が
            // 0件になる（エラーにはしない。他の基点が解決できないときと同じ
            // 「安全側に倒して黙って空にする」方針を踏襲する）。レジストリ
            // 照会による追従は本対応のスコープ外とし、必要になれば別 Issue と
            // する（A4 / Issue #43）。
            base: KnownDir::DeliveryOptimizationCache,
            match_kind: MatchKind::All,
            needs_admin: true,
            // system_temp / windows_update_cache と同じ理由で Review（既定
            // OFF・自動推奨なし）。解放できる容量が大きいことは「既定 ON に
            // してよい理由」にはならない（NF-SAF-01）。この Safety を Safe や
            // Caution に上げてはならない。
            safety: Safety::Review,
            age_threshold_days: None,
            // system_temp と同じ理由で None のまま。
            large_file_threshold_bytes: None,
        },
    ]
}

/// 走査・削除の対象となるルールを返す（F-SCAN-04 / A1 / Issue #40）。
///
/// `elevated` が `false` のときは `needs_admin` なルールを除外する。`true`
/// （＝A2 の昇格を経てユーザーが明示的に管理者権限を与えた）ときは含める。
/// 判定は [`Rule::is_permitted`] に一本化し、GUI 側の「管理者権限が必要」
/// 表示（`gui/src/view.rs` の `future_rules`）とは厳密な補集合の関係を保つ。
pub fn scannable_rules(config: &Config, elevated: bool) -> Vec<Rule> {
    builtin_rules(config)
        .into_iter()
        .filter(|r| r.is_permitted(elevated))
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
        let rules = builtin_rules(&Config::default());
        let ids: HashSet<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        let expected: HashSet<&str> = [
            "user_temp",
            "browser_cache",
            "thumbnail_cache",
            "recycle_bin",
            "old_logs",
            "old_downloads",
            "system_temp",
            "windows_update_cache",
            "delivery_optimization_cache",
        ]
        .into_iter()
        .collect();
        assert_eq!(ids, expected);
        assert_eq!(rules.len(), expected.len(), "id が重複していない");
    }

    #[test]
    fn every_rule_has_non_empty_id_label_description() {
        for rule in builtin_rules(&Config::default()) {
            assert!(!rule.id.is_empty());
            assert!(!rule.label.is_empty());
            assert!(!rule.description.is_empty());
        }
    }

    #[test]
    fn builtin_rules_cover_all_safety_levels() {
        let rules = builtin_rules(&Config::default());
        assert!(rules.iter().any(|r| r.safety == Safety::Safe));
        assert!(rules.iter().any(|r| r.safety == Safety::Caution));
        assert!(rules.iter().any(|r| r.safety == Safety::Review));
    }

    #[test]
    fn needs_admin_rules_are_excluded_unless_elevated() {
        let config = Config::default();
        let rules = builtin_rules(&config);
        let mut admin_ids: Vec<&str> = rules
            .iter()
            .filter(|r| r.needs_admin)
            .map(|r| r.id.as_str())
            .collect();
        admin_ids.sort_unstable();
        assert_eq!(
            admin_ids,
            vec![
                "delivery_optimization_cache",
                "system_temp",
                "windows_update_cache",
            ]
        );

        let not_elevated = scannable_rules(&config, false);
        assert_eq!(not_elevated.len(), rules.len() - admin_ids.len());
        assert!(
            not_elevated
                .iter()
                .all(|r| !admin_ids.contains(&r.id.as_str()))
        );

        let elevated = scannable_rules(&config, true);
        assert_eq!(elevated.len(), rules.len(), "昇格時は全ルールが対象になる");
        for id in &admin_ids {
            assert!(elevated.iter().any(|r| &r.id == id));
        }
    }

    #[test]
    fn admin_rules_are_all_review() {
        // needs_admin なルールはすべて Safety::Review であること。管理者
        // 領域を既定 ON にしないための原則（system_temp のコメント参照）が、
        // ルールが増えても崩れないことの一般化した退行テスト（A3 / Issue #42、
        // A4 / Issue #43）。
        for rule in builtin_rules(&Config::default())
            .into_iter()
            .filter(|r| r.needs_admin)
        {
            assert_eq!(
                rule.safety,
                Safety::Review,
                "id={} は needs_admin なのに Safety::Review でない",
                rule.id
            );
        }
    }

    #[test]
    fn is_permitted_truth_table() {
        let admin_rule = rule_with(true, Safety::Review);
        let normal_rule = rule_with(false, Safety::Safe);

        assert!(!admin_rule.is_permitted(false));
        assert!(admin_rule.is_permitted(true));
        assert!(normal_rule.is_permitted(false));
        assert!(normal_rule.is_permitted(true));
    }

    #[test]
    fn may_touch_admin_area_truth_table() {
        let admin_rule = rule_with(true, Safety::Review);
        let normal_rule = rule_with(false, Safety::Safe);

        // needs_admin なルールは、昇格しているときだけ管理者領域に触れてよい。
        assert!(!admin_rule.may_touch_admin_area(false));
        assert!(admin_rule.may_touch_admin_area(true));

        // needs_admin でないルールは、昇格していても管理者領域には触れられ
        // ない（環境変数の設定ミス等に対する安全網。Issue #40）。
        assert!(!normal_rule.may_touch_admin_area(false));
        assert!(!normal_rule.may_touch_admin_area(true));
    }

    #[test]
    fn system_temp_stays_review() {
        // system_temp の Safety を Safe / Caution へ引き上げてはならない
        // （NF-SAF-01。管理者領域を既定 ON にしないための二重防御の一部）。
        let rules = builtin_rules(&Config::default());
        let system_temp = rules.iter().find(|r| r.id == "system_temp").unwrap();
        assert_eq!(system_temp.safety, Safety::Review);
    }

    fn rule_with(needs_admin: bool, safety: Safety) -> Rule {
        Rule {
            id: "test_rule".to_string(),
            label: "テストルール".to_string(),
            description: "テスト用".to_string(),
            base: crate::platform::KnownDir::UserTemp,
            match_kind: MatchKind::All,
            needs_admin,
            safety,
            age_threshold_days: None,
            large_file_threshold_bytes: None,
        }
    }

    #[test]
    fn age_thresholds_match_the_specification() {
        for rule in builtin_rules(&Config::default()) {
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
        for rule in builtin_rules(&Config::default()) {
            assert!(!rule.label.contains(':'), "id={}", rule.id);
            assert!(!rule.label.contains('\\'), "id={}", rule.id);
            assert!(!rule.description.contains(':'), "id={}", rule.id);
            assert!(!rule.description.contains('\\'), "id={}", rule.id);
        }
    }

    #[test]
    fn large_file_threshold_bytes_is_populated_from_config() {
        // C2 / Issue #48: Config::large_file_threshold_bytes をルールへ
        // 持ち込むルール（user_temp / browser_cache / old_logs / old_downloads）
        // では、設定値がそのまま反映されること。
        let config = Config {
            large_file_threshold_bytes: 12_345,
            ..Config::default()
        };
        let rules = builtin_rules(&config);

        let with_threshold = ["user_temp", "browser_cache", "old_logs", "old_downloads"];
        for id in with_threshold {
            let rule = rules.iter().find(|r| r.id == id).unwrap();
            assert_eq!(rule.large_file_threshold_bytes, Some(12_345), "id={id}");
        }
    }
}
