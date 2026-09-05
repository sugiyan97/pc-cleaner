//! 組み込みルールの上書き・追加ルールの外部ファイル化（D4 / Issue #54）。
//!
//! `<config_dir>/rules.json` を読み、[`crate::rule::builtin_rules`] へ
//! 「上書き（`overrides`）」と「追加（`rules`）」を適用した解決済みの
//! [`RuleSet`] を作る。許可リスト方式（NF-SAF-04）はここでも維持する：
//!
//! - `needs_admin` はファイルから一切変更できない（常に `false` のまま）。
//! - ユーザー定義ルールの走査基点（`base`）は、管理者権限領域でもゴミ箱
//!   でもない5つの `KnownDir` のみを**許可リスト**として受け付ける
//!   （[`ALLOWED_USER_RULE_BASES`]）。新しい管理者権限領域が将来
//!   `KnownDir` に追加されても、このリストに明示的に足さない限り
//!   ユーザー定義ルールの基点にはできない（否定リストではなく許可リスト
//!   にしているのは、まさにこの「追加し忘れても安全側に倒れる」ため）。
//! - `Safety` はリスクを下げる方向には変更できない（`overrides`）。
//!   ユーザー定義の新規ルールは `Safety::Safe` を指定できない（既定
//!   `Review`、`Caution` まで昇格可）。`Safe` は「起動直後に選択済み」を
//!   意味し（F-REC-02）、未検証のルールをその動線に乗せるのは NF-SAF-01
//!   と衝突するため。
//!
//! ファイルが存在しない・壊れている・スキーマバージョンが未知の場合は、
//! 組み込みルールのみで安全に続行し、理由を [`RuleSetIssue`] として返す
//! （`config.rs` の「壊れたら黙って既定へ」とも `history.rs` の「壊れたら
//! `Err`」とも異なる、3つ目の方針：一部の記述が誤っていても、その項目
//! だけを落として残りは活かす）。

use crate::config::{AGE_THRESHOLD_MAX_DAYS, AGE_THRESHOLD_MIN_DAYS, Config};
use crate::platform::{KnownDir, Platform};
use crate::rule::{MatchKind, Rule, Safety};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// 本モジュールが理解できるルール定義ファイルのスキーマバージョン。
/// ファイルの `schema_version` がこれより大きい場合は解釈せず、組み込み
/// ルールのみで続行する（将来のフィールド追加が過去のツールを誤動作
/// させないため）。
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// ルール定義ファイル名。
const RULES_FILE_NAME: &str = "rules.json";

/// ユーザー定義ルールの `id` に許可する最大長。
const ID_MAX_LEN: usize = 64;
/// `label` に許可する最大文字数。
const LABEL_MAX_LEN: usize = 200;
/// `description` に許可する最大文字数。
const DESCRIPTION_MAX_LEN: usize = 2000;
/// ユーザー定義ルールの最大件数（走査コストの暴走防止。`scan.rs` の
/// `MAX_DEPTH` と同じ「防御的措置」の位置づけ）。
pub const MAX_USER_RULES: usize = 32;

/// ユーザー定義ルールの `base` に許可する `KnownDir` の一覧（許可リスト）。
/// 管理者権限領域（`SystemTemp` / `WindowsUpdateCache` /
/// `DeliveryOptimizationCache`）と `RecycleBin`（`delete.rs` が特別扱いする
/// コンテナ）は意図的に含めない。
const ALLOWED_USER_RULE_BASES: &[(&str, KnownDir)] = &[
    ("UserTemp", KnownDir::UserTemp),
    ("LocalAppData", KnownDir::LocalAppData),
    ("Cache", KnownDir::Cache),
    ("Downloads", KnownDir::Downloads),
    ("ThumbnailCache", KnownDir::ThumbnailCache),
];

/// ルール定義ファイルの検証で見つかった問題。致命的ではなく、該当する
/// ルール（または該当箇所）だけを無視して残りは活かす（モジュール doc
/// 参照）。CLI/GUI はこれを警告として提示する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSetIssue {
    /// 関連するルール `id`。ファイル全体に関する問題（未知のスキーマ
    /// バージョン等）の場合は `None`。
    pub rule_id: Option<String>,
    /// 人が読める説明文。
    pub message: String,
}

/// 解決済みのルール集合（組み込み + 検証済みのユーザー定義・上書き）。
#[derive(Debug, Clone)]
pub struct RuleSet {
    rules: Vec<Rule>,
    issues: Vec<RuleSetIssue>,
}

impl RuleSet {
    /// 組み込みルールのみ（ファイルを読まない）。テスト・フォールバック用。
    pub fn builtin(config: &Config) -> Self {
        RuleSet {
            rules: crate::rule::builtin_rules(config),
            issues: Vec::new(),
        }
    }

    /// `<config_dir>/rules.json` を読み込んで解決する。I/O はここだけで
    /// 行う。`Config::user_rules_enabled` が `false` の場合はファイルを
    /// 読まず、組み込みルールのみを返す。
    pub fn load(platform: &dyn Platform, config: &Config) -> Self {
        if !config.user_rules_enabled {
            return RuleSet::builtin(config);
        }

        let Some(path) = rules_file_path(platform) else {
            return RuleSet::builtin(config);
        };

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return RuleSet::builtin(config);
            }
            Err(e) => {
                let mut set = RuleSet::builtin(config);
                set.issues.push(RuleSetIssue {
                    rule_id: None,
                    message: format!(
                        "ルール定義ファイル（{}）を読み込めませんでした: {e}。組み込みルールのみで続行します。",
                        path.display()
                    ),
                });
                return set;
            }
        };

        let file: RuleFile = match serde_json::from_str(&contents) {
            Ok(file) => file,
            Err(e) => {
                let mut set = RuleSet::builtin(config);
                set.issues.push(RuleSetIssue {
                    rule_id: None,
                    message: format!(
                        "ルール定義ファイル（{}）の解析に失敗しました: {e}。組み込みルールのみで続行します。",
                        path.display()
                    ),
                });
                return set;
            }
        };

        if file.schema_version > CURRENT_SCHEMA_VERSION {
            let mut set = RuleSet::builtin(config);
            set.issues.push(RuleSetIssue {
                rule_id: None,
                message: format!(
                    "ルール定義ファイルの schema_version（{}）はこのバージョンの pc-cleaner では未対応です（対応: {}以下）。組み込みルールのみで続行します。",
                    file.schema_version, CURRENT_SCHEMA_VERSION
                ),
            });
            return set;
        }

        let (rules, issues) = resolve_rules(config, Some(&file));
        RuleSet { rules, issues }
    }

    /// 指定した安全度のルールだけを返す。`needs_admin` なルールは `elevated`
    /// が `true` でない限り除外する（`scan::rules_for_safeties` と同じ
    /// 判定。ユーザー定義ルールは常に `needs_admin == false` のため、この
    /// フィルタで弾かれることはない）。
    pub fn for_safeties(&self, safeties: &[Safety], elevated: bool) -> Vec<Rule> {
        self.rules
            .iter()
            .filter(|r| r.is_permitted(elevated))
            .filter(|r| safeties.contains(&r.safety))
            .cloned()
            .collect()
    }

    /// 解決済みの全ルール（`needs_admin` の絞り込み前）。
    pub fn all(&self) -> &[Rule] {
        &self.rules
    }

    /// ファイル読み込み・検証で見つかった問題。
    pub fn issues(&self) -> &[RuleSetIssue] {
        &self.issues
    }

    /// ユーザー定義（組み込みにない）ルールの件数。GUI/CLI の表示用。
    pub fn user_defined_rule_count(&self) -> usize {
        self.rules.iter().filter(|r| r.is_user_defined).count()
    }
}

/// `Platform::config_dir()` を用いてルール定義ファイルのフルパス
/// （`<config_dir>/rules.json`）を解決する。`config_dir` が解決できない
/// 場合は `None`。
pub fn rules_file_path(platform: &dyn Platform) -> Option<PathBuf> {
    platform.config_dir().map(|dir| dir.join(RULES_FILE_NAME))
}

// ---------------------------------------------------------------------
// ファイルの生スキーマ（Deserialize のみ）
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct RuleFile {
    schema_version: u32,
    overrides: HashMap<String, RuleOverride>,
    rules: Vec<RawUserRule>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct RuleOverride {
    safety: Option<String>,
    age_threshold_days: Option<u64>,
    enabled: Option<bool>,
    /// 宣言されていないフィールド（`needs_admin` 等）を捕捉するためだけの
    /// 受け皿。`needs_admin` が書かれていた場合に「無視した」旨を警告する
    /// ために使う（本フィールド自体が上書き先を持つことはない）。
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct RawUserRule {
    id: Option<String>,
    label: Option<String>,
    description: Option<String>,
    base: Option<String>,
    #[serde(rename = "match")]
    match_kind: Option<RawMatchKind>,
    safety: Option<String>,
    age_threshold_days: Option<u64>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum RawMatchKind {
    All,
    Extension {
        #[serde(default)]
        extensions: Vec<String>,
    },
    OlderThan,
}

// ---------------------------------------------------------------------
// 解決（純粋関数。I/O なし）
// ---------------------------------------------------------------------

/// 組み込みルールへ `file` の内容（`overrides` / `rules`）を適用し、解決済み
/// ルール一覧と検出した問題を返す純粋関数。`file` が `None` の場合は組み込み
/// ルールをそのまま返す（問題なし）。
fn resolve_rules(config: &Config, file: Option<&RuleFile>) -> (Vec<Rule>, Vec<RuleSetIssue>) {
    let mut rules = crate::rule::builtin_rules(config);
    let mut issues = Vec::new();

    let Some(file) = file else {
        return (rules, issues);
    };

    // ---- overrides：組み込みルールの上書き ----
    for (rule_id, over) in &file.overrides {
        if over.extra.contains_key("needs_admin") {
            issues.push(RuleSetIssue {
                rule_id: Some(rule_id.clone()),
                message: format!(
                    "'{rule_id}' の overrides.needs_admin は無視されました（needs_admin はファイルから変更できません）"
                ),
            });
        }

        let Some(rule) = rules.iter_mut().find(|r| &r.id == rule_id) else {
            issues.push(RuleSetIssue {
                rule_id: Some(rule_id.clone()),
                message: format!(
                    "組み込みルール '{rule_id}' が見つかりません（overrides はルールの追加には使えません。新規ルールは rules に書いてください）"
                ),
            });
            continue;
        };

        if let Some(safety_str) = &over.safety {
            match parse_safety(safety_str) {
                Ok(requested) => {
                    if requested < rule.safety {
                        issues.push(RuleSetIssue {
                            rule_id: Some(rule_id.clone()),
                            message: format!(
                                "'{rule_id}' の safety をリスクの低い方向（{:?} → {:?}）には変更できません",
                                rule.safety, requested
                            ),
                        });
                    } else {
                        rule.safety = requested;
                    }
                }
                Err(msg) => issues.push(RuleSetIssue {
                    rule_id: Some(rule_id.clone()),
                    message: format!("'{rule_id}' の safety: {msg}"),
                }),
            }
        }

        if let Some(days) = over.age_threshold_days {
            rule.age_threshold_days =
                Some(days.clamp(AGE_THRESHOLD_MIN_DAYS, AGE_THRESHOLD_MAX_DAYS));
        }

        if over.enabled == Some(false) {
            let disabled_id = rule.id.clone();
            rules.retain(|r| r.id != disabled_id);
        }
    }

    // ---- rules：ユーザー定義ルールの追加 ----
    let existing_ids: HashSet<String> = rules.iter().map(|r| r.id.clone()).collect();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for raw in file.rules.iter().take(MAX_USER_RULES) {
        match build_user_rule(raw, &existing_ids, &seen_ids) {
            Ok(rule) => {
                seen_ids.insert(rule.id.clone());
                rules.push(rule);
            }
            Err(msg) => issues.push(RuleSetIssue {
                rule_id: raw.id.clone(),
                message: msg,
            }),
        }
    }
    if file.rules.len() > MAX_USER_RULES {
        issues.push(RuleSetIssue {
            rule_id: None,
            message: format!(
                "ユーザー定義ルールは最大 {MAX_USER_RULES} 件までです（{}件のうち超過分は無視しました）",
                file.rules.len()
            ),
        });
    }

    (rules, issues)
}

fn parse_safety(value: &str) -> Result<Safety, String> {
    match value {
        "Safe" => Ok(Safety::Safe),
        "Caution" => Ok(Safety::Caution),
        "Review" => Ok(Safety::Review),
        other => Err(format!("'{other}' は不明です（Safe / Caution / Review）")),
    }
}

fn parse_allowed_base(name: &str) -> Result<KnownDir, String> {
    ALLOWED_USER_RULE_BASES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, k)| *k)
        .ok_or_else(|| {
            let allowed: Vec<&str> = ALLOWED_USER_RULE_BASES.iter().map(|(n, _)| *n).collect();
            format!(
                "base '{name}' は使用できません（許可: {}）",
                allowed.join(", ")
            )
        })
}

fn validate_id(
    id: &str,
    existing_ids: &HashSet<String>,
    seen_ids: &HashSet<String>,
) -> Result<(), String> {
    if id.is_empty()
        || id.len() > ID_MAX_LEN
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!(
            "id '{id}' は1〜{ID_MAX_LEN}文字の英小文字・数字・アンダースコアのみ使用できます"
        ));
    }
    if existing_ids.contains(id) {
        return Err(format!(
            "id '{id}' は組み込みルールと重複しています（上書きは overrides を使ってください）"
        ));
    }
    if seen_ids.contains(id) {
        return Err(format!("id '{id}' はユーザー定義ルール内で重複しています"));
    }
    Ok(())
}

/// `label` / `description` の共通検証。生パスの混入防止（`rule.rs` の静的
/// テスト `rule_text_does_not_contain_a_raw_windows_path` の実行時版）も
/// ここで行う。
fn validate_text(value: &str, field: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{field} を空にすることはできません"));
    }
    if value.chars().count() > max_len {
        return Err(format!("{field} は{max_len}文字以内にしてください"));
    }
    if value.contains(':') || value.contains('\\') {
        return Err(format!(
            "{field} にパスらしき文字（':' や '\\'）を含めることはできません"
        ));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(format!("{field} に制御文字を含めることはできません"));
    }
    Ok(())
}

fn normalize_extension(ext: &str) -> Result<String, String> {
    let trimmed = ext.trim_start_matches('.');
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains(':')
        || trimmed.contains('*')
        || trimmed.chars().any(|c| c.is_whitespace())
    {
        return Err(format!("拡張子 '{ext}' は使用できません"));
    }
    Ok(trimmed.to_lowercase())
}

/// `raw` を検証し、走査・削除の対象になりうる [`Rule`] を作る。
/// `needs_admin` は常に `false`（ファイルからは変更できない）。
fn build_user_rule(
    raw: &RawUserRule,
    existing_ids: &HashSet<String>,
    seen_ids: &HashSet<String>,
) -> Result<Rule, String> {
    if raw.extra.contains_key("needs_admin") {
        return Err(
            "needs_admin はユーザー定義ルールでは指定できません（常に false 扱いです）".to_string(),
        );
    }

    let id = raw.id.as_deref().ok_or("id は必須です")?;
    validate_id(id, existing_ids, seen_ids)?;

    let label = raw.label.as_deref().ok_or("label は必須です")?;
    validate_text(label, "label", LABEL_MAX_LEN)?;

    let description = raw.description.as_deref().ok_or("description は必須です")?;
    validate_text(description, "description", DESCRIPTION_MAX_LEN)?;

    let base_name = raw.base.as_deref().ok_or("base は必須です")?;
    let base = parse_allowed_base(base_name)?;

    let safety = match raw.safety.as_deref() {
        None => Safety::Review,
        Some("Safe") => {
            return Err(
                "ユーザー定義ルールに safety: Safe は指定できません（既定 Review、Caution まで昇格可）"
                    .to_string(),
            );
        }
        Some(other) => parse_safety(other)?,
    };

    let raw_match = raw.match_kind.as_ref().ok_or("match は必須です")?;
    let match_kind = match raw_match {
        RawMatchKind::All => MatchKind::All,
        RawMatchKind::Extension { extensions } => {
            if extensions.is_empty() {
                return Err(
                    "match.kind が Extension の場合、extensions は1件以上必要です".to_string(),
                );
            }
            let normalized: Result<Vec<String>, String> =
                extensions.iter().map(|e| normalize_extension(e)).collect();
            MatchKind::Extension(normalized?)
        }
        RawMatchKind::OlderThan => {
            if raw.age_threshold_days.is_none() {
                return Err(
                    "match.kind が OlderThan の場合、age_threshold_days が必須です".to_string(),
                );
            }
            MatchKind::OlderThan
        }
    };

    let age_threshold_days = raw
        .age_threshold_days
        .map(|d| d.clamp(AGE_THRESHOLD_MIN_DAYS, AGE_THRESHOLD_MAX_DAYS));

    Ok(Rule {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        base,
        match_kind,
        needs_admin: false,
        safety,
        age_threshold_days,
        large_file_threshold_bytes: None,
        is_user_defined: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{ElevateError, ElevateResult, PlatformError};
    use std::path::Path;

    struct FakePlatform {
        config_dir: Option<PathBuf>,
    }

    impl Platform for FakePlatform {
        fn known_dir(&self, _kind: KnownDir) -> Option<PathBuf> {
            None
        }
        fn to_trash(&self, _path: &Path) -> crate::platform::Result<()> {
            Err(PlatformError::Unsupported("to_trash"))
        }
        fn requires_admin(&self, _path: &Path) -> bool {
            false
        }
        fn config_dir(&self) -> Option<PathBuf> {
            self.config_dir.clone()
        }
        fn is_elevated(&self) -> bool {
            false
        }
        fn elevate(&self, _args: &[String]) -> ElevateResult {
            Err(ElevateError::Unsupported)
        }
    }

    fn parse(json: &str) -> RuleFile {
        serde_json::from_str(json).unwrap()
    }

    // ---- resolve_rules: 安全性の要（最重要の退行テスト群） ----

    #[test]
    fn admin_area_bases_are_rejected_for_user_defined_rules() {
        for base in [
            "SystemTemp",
            "WindowsUpdateCache",
            "DeliveryOptimizationCache",
        ] {
            let file = parse(&format!(
                r#"{{"rules":[{{"id":"evil","label":"x","description":"y","base":"{base}","match":{{"kind":"All"}}}}]}}"#
            ));
            let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
            assert!(
                !rules.iter().any(|r| r.id == "evil"),
                "base={base} は採用されてはならない"
            );
            assert!(!issues.is_empty(), "base={base} は issue を出すはず");
        }
    }

    #[test]
    fn recycle_bin_base_is_rejected_for_user_defined_rules() {
        let file = parse(
            r#"{"rules":[{"id":"evil","label":"x","description":"y","base":"RecycleBin","match":{"kind":"All"}}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(!rules.iter().any(|r| r.id == "evil"));
        assert!(!issues.is_empty());
    }

    #[test]
    fn needs_admin_in_user_rule_is_ignored_and_forced_false() {
        let file = parse(
            r#"{"rules":[{"id":"my_rule","label":"x","description":"y","base":"UserTemp","match":{"kind":"All"},"needs_admin":true}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(
            !rules.iter().any(|r| r.id == "my_rule"),
            "needs_admin を書いた時点でルール自体を拒否する"
        );
        assert!(issues.iter().any(|i| i.message.contains("needs_admin")));
    }

    #[test]
    fn overrides_cannot_set_needs_admin() {
        let file = parse(r#"{"overrides":{"system_temp":{"needs_admin":false}}}"#);
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        let system_temp = rules.iter().find(|r| r.id == "system_temp").unwrap();
        assert!(system_temp.needs_admin, "overrides では変更されない");
        assert!(issues.iter().any(|i| i.message.contains("needs_admin")));
    }

    #[test]
    fn overrides_cannot_lower_safety_below_current() {
        let file = parse(r#"{"overrides":{"system_temp":{"safety":"Safe"}}}"#);
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        let system_temp = rules.iter().find(|r| r.id == "system_temp").unwrap();
        assert_eq!(
            system_temp.safety,
            Safety::Review,
            "system_temp を Safe へは下げられない"
        );
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn overrides_can_raise_safety() {
        let file = parse(r#"{"overrides":{"user_temp":{"safety":"Caution"}}}"#);
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        let user_temp = rules.iter().find(|r| r.id == "user_temp").unwrap();
        assert_eq!(user_temp.safety, Safety::Caution);
        assert!(issues.is_empty());
    }

    #[test]
    fn overrides_can_disable_a_builtin_rule() {
        let file = parse(r#"{"overrides":{"old_logs":{"enabled":false}}}"#);
        let (rules, _issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(!rules.iter().any(|r| r.id == "old_logs"));
    }

    #[test]
    fn overrides_can_adjust_age_threshold_with_clamping() {
        let file = parse(r#"{"overrides":{"old_logs":{"age_threshold_days":99999}}}"#);
        let (rules, _issues) = resolve_rules(&Config::default(), Some(&file));
        let old_logs = rules.iter().find(|r| r.id == "old_logs").unwrap();
        assert_eq!(old_logs.age_threshold_days, Some(AGE_THRESHOLD_MAX_DAYS));
    }

    #[test]
    fn unknown_override_rule_id_produces_an_issue_but_others_still_apply() {
        let file = parse(
            r#"{"overrides":{"no_such_rule":{"safety":"Caution"},"user_temp":{"safety":"Caution"}}}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(
            issues
                .iter()
                .any(|i| i.rule_id.as_deref() == Some("no_such_rule"))
        );
        let user_temp = rules.iter().find(|r| r.id == "user_temp").unwrap();
        assert_eq!(user_temp.safety, Safety::Caution, "他の override は生きる");
    }

    // ---- ユーザー定義ルールの追加 ----

    #[test]
    fn valid_user_rule_with_extension_match_is_added() {
        let file = parse(
            r#"{"rules":[{"id":"my_app_cache","label":"MyApp","description":"desc","base":"LocalAppData","match":{"kind":"Extension","extensions":[".TMP",".cache"]},"safety":"Caution"}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(issues.is_empty());
        let rule = rules.iter().find(|r| r.id == "my_app_cache").unwrap();
        assert!(rule.is_user_defined);
        assert!(!rule.needs_admin);
        assert_eq!(rule.safety, Safety::Caution);
        assert_eq!(rule.base, KnownDir::LocalAppData);
        assert_eq!(
            rule.match_kind,
            MatchKind::Extension(vec!["tmp".to_string(), "cache".to_string()]),
            "拡張子は正規化（先頭ドット除去・小文字化）される"
        );
    }

    #[test]
    fn user_rule_defaults_to_review_safety() {
        let file = parse(
            r#"{"rules":[{"id":"my_rule","label":"x","description":"y","base":"UserTemp","match":{"kind":"All"}}]}"#,
        );
        let (rules, _issues) = resolve_rules(&Config::default(), Some(&file));
        assert_eq!(
            rules.iter().find(|r| r.id == "my_rule").unwrap().safety,
            Safety::Review
        );
    }

    #[test]
    fn user_rule_cannot_request_safe_safety() {
        let file = parse(
            r#"{"rules":[{"id":"my_rule","label":"x","description":"y","base":"UserTemp","match":{"kind":"All"},"safety":"Safe"}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(!rules.iter().any(|r| r.id == "my_rule"));
        assert!(issues.iter().any(|i| i.message.contains("Safe")));
    }

    #[test]
    fn user_rule_id_colliding_with_builtin_is_rejected() {
        let file = parse(
            r#"{"rules":[{"id":"user_temp","label":"x","description":"y","base":"UserTemp","match":{"kind":"All"}}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert_eq!(
            rules.iter().filter(|r| r.id == "user_temp").count(),
            1,
            "組み込みの user_temp が重複して追加されない"
        );
        assert!(!issues.is_empty());
    }

    #[test]
    fn duplicate_user_rule_ids_are_rejected() {
        let file = parse(
            r#"{"rules":[
                {"id":"dup","label":"a","description":"y","base":"UserTemp","match":{"kind":"All"}},
                {"id":"dup","label":"b","description":"y","base":"UserTemp","match":{"kind":"All"}}
            ]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert_eq!(rules.iter().filter(|r| r.id == "dup").count(), 1);
        assert!(!issues.is_empty());
    }

    #[test]
    fn invalid_id_characters_are_rejected() {
        let file = parse(
            r#"{"rules":[{"id":"My Rule!","label":"x","description":"y","base":"UserTemp","match":{"kind":"All"}}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(rules.iter().all(|r| r.id != "My Rule!"));
        assert!(!issues.is_empty());
    }

    #[test]
    fn label_containing_a_path_like_string_is_rejected() {
        let file = parse(
            r#"{"rules":[{"id":"my_rule","label":"C:\\Users","description":"y","base":"UserTemp","match":{"kind":"All"}}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(!rules.iter().any(|r| r.id == "my_rule"));
        assert!(!issues.is_empty());
    }

    #[test]
    fn older_than_without_age_threshold_is_rejected() {
        let file = parse(
            r#"{"rules":[{"id":"my_rule","label":"x","description":"y","base":"Downloads","match":{"kind":"OlderThan"}}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(!rules.iter().any(|r| r.id == "my_rule"));
        assert!(!issues.is_empty());
    }

    #[test]
    fn extension_match_without_any_extensions_is_rejected() {
        let file = parse(
            r#"{"rules":[{"id":"my_rule","label":"x","description":"y","base":"Cache","match":{"kind":"Extension","extensions":[]}}]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(!rules.iter().any(|r| r.id == "my_rule"));
        assert!(!issues.is_empty());
    }

    #[test]
    fn unknown_base_name_produces_an_issue_but_other_rules_still_apply() {
        let file = parse(
            r#"{"rules":[
                {"id":"bad","label":"x","description":"y","base":"NoSuchDir","match":{"kind":"All"}},
                {"id":"good","label":"x","description":"y","base":"UserTemp","match":{"kind":"All"}}
            ]}"#,
        );
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        assert!(!rules.iter().any(|r| r.id == "bad"));
        assert!(rules.iter().any(|r| r.id == "good"), "他のルールは生きる");
        assert!(issues.iter().any(|i| i.rule_id.as_deref() == Some("bad")));
    }

    #[test]
    fn user_rule_count_beyond_max_is_truncated_with_an_issue() {
        let rules_json: Vec<String> = (0..MAX_USER_RULES + 5)
            .map(|i| {
                format!(
                    r#"{{"id":"rule_{i}","label":"x","description":"y","base":"UserTemp","match":{{"kind":"All"}}}}"#
                )
            })
            .collect();
        let file = parse(&format!(r#"{{"rules":[{}]}}"#, rules_json.join(",")));
        let (rules, issues) = resolve_rules(&Config::default(), Some(&file));
        let user_defined_count = rules.iter().filter(|r| r.is_user_defined).count();
        assert_eq!(user_defined_count, MAX_USER_RULES);
        assert!(issues.iter().any(|i| i.message.contains("最大")));
    }

    #[test]
    fn age_threshold_is_clamped_for_user_rules() {
        let file = parse(
            r#"{"rules":[{"id":"my_rule","label":"x","description":"y","base":"Downloads","match":{"kind":"OlderThan"},"age_threshold_days":0}]}"#,
        );
        let (rules, _issues) = resolve_rules(&Config::default(), Some(&file));
        let rule = rules.iter().find(|r| r.id == "my_rule").unwrap();
        assert_eq!(rule.age_threshold_days, Some(AGE_THRESHOLD_MIN_DAYS));
    }

    #[test]
    fn no_file_means_builtin_rules_unchanged() {
        let (rules, issues) = resolve_rules(&Config::default(), None);
        assert_eq!(rules, crate::rule::builtin_rules(&Config::default()));
        assert!(issues.is_empty());
    }

    // ---- RuleSet::load ----

    #[test]
    fn load_without_config_dir_falls_back_to_builtin() {
        let platform = FakePlatform { config_dir: None };
        let set = RuleSet::load(&platform, &Config::default());
        assert_eq!(set.all(), crate::rule::builtin_rules(&Config::default()));
        assert!(set.issues().is_empty());
    }

    #[test]
    fn load_when_file_missing_falls_back_to_builtin_without_issue() {
        let dir = tempfile::tempdir().unwrap();
        let platform = FakePlatform {
            config_dir: Some(dir.path().to_path_buf()),
        };
        let set = RuleSet::load(&platform, &Config::default());
        assert_eq!(set.all(), crate::rule::builtin_rules(&Config::default()));
        assert!(set.issues().is_empty());
    }

    #[test]
    fn load_when_file_is_corrupt_falls_back_to_builtin_with_an_issue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rules.json"), "not valid json").unwrap();
        let platform = FakePlatform {
            config_dir: Some(dir.path().to_path_buf()),
        };
        let set = RuleSet::load(&platform, &Config::default());
        assert_eq!(set.all(), crate::rule::builtin_rules(&Config::default()));
        assert_eq!(set.issues().len(), 1);
    }

    #[test]
    fn load_when_schema_version_is_unsupported_falls_back_to_builtin_with_an_issue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rules.json"), r#"{"schema_version":999}"#).unwrap();
        let platform = FakePlatform {
            config_dir: Some(dir.path().to_path_buf()),
        };
        let set = RuleSet::load(&platform, &Config::default());
        assert_eq!(set.all(), crate::rule::builtin_rules(&Config::default()));
        assert_eq!(set.issues().len(), 1);
    }

    #[test]
    fn load_when_disabled_does_not_read_the_file() {
        let dir = tempfile::tempdir().unwrap();
        // 壊れたファイルを置いても、無効化されていれば読まれない（issue も出ない）。
        std::fs::write(dir.path().join("rules.json"), "not valid json").unwrap();
        let platform = FakePlatform {
            config_dir: Some(dir.path().to_path_buf()),
        };
        let config = Config {
            user_rules_enabled: false,
            ..Config::default()
        };
        let set = RuleSet::load(&platform, &config);
        assert_eq!(set.all(), crate::rule::builtin_rules(&config));
        assert!(set.issues().is_empty());
    }

    #[test]
    fn load_applies_valid_overrides_and_user_rules_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rules.json"),
            r#"{
                "schema_version": 1,
                "overrides": {"old_logs": {"safety": "Review"}},
                "rules": [
                    {"id":"my_app_cache","label":"MyApp","description":"desc","base":"LocalAppData","match":{"kind":"All"}}
                ]
            }"#,
        )
        .unwrap();
        let platform = FakePlatform {
            config_dir: Some(dir.path().to_path_buf()),
        };
        let set = RuleSet::load(&platform, &Config::default());
        assert!(set.issues().is_empty());
        assert_eq!(
            set.all()
                .iter()
                .find(|r| r.id == "old_logs")
                .unwrap()
                .safety,
            Safety::Review
        );
        assert!(set.all().iter().any(|r| r.id == "my_app_cache"));
        assert_eq!(set.user_defined_rule_count(), 1);
    }

    // ---- RuleSet::for_safeties: 昇格ゲートが崩れていないこと ----

    #[test]
    fn for_safeties_still_excludes_needs_admin_rules_unless_elevated() {
        let set = RuleSet::builtin(&Config::default());
        let not_elevated = set.for_safeties(&[Safety::Review], false);
        assert!(!not_elevated.iter().any(|r| r.needs_admin));
        let elevated = set.for_safeties(&[Safety::Review], true);
        assert!(elevated.iter().any(|r| r.id == "system_temp"));
    }

    #[test]
    fn for_safeties_includes_user_defined_rules_regardless_of_elevation() {
        let file = parse(
            r#"{"rules":[{"id":"my_rule","label":"x","description":"y","base":"UserTemp","match":{"kind":"All"},"safety":"Caution"}]}"#,
        );
        let (rules, _issues) = resolve_rules(&Config::default(), Some(&file));
        let set = RuleSet {
            rules,
            issues: Vec::new(),
        };
        let not_elevated = set.for_safeties(&[Safety::Caution], false);
        assert!(not_elevated.iter().any(|r| r.id == "my_rule"));
    }
}
