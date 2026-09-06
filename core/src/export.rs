//! 走査結果・削除計画のエクスポート（JSON/CSV、D2 / Issue #52）。
//!
//! 「予定（走査結果・削除計画）」を出力する責務に限定する。「実績」（実際に
//! 削除した結果）は [`crate::audit`]（D1 / Issue #51）が別途担うため、両者の
//! 内容は重複しない。
//!
//! ここに定義する関数はすべて `&ScanEntry` / `&DeletePlan` を**不変参照**で
//! 受け取るだけで、`DeletePlan` を消費しない。[`crate::delete::execute`] は
//! `DeletePlan` を値で消費する設計（F-DEL-01）のため、本モジュールの関数を
//! 経由して削除が実行される経路は存在しない（安全性ゲートとは無関係な
//! 読み取り専用の整形処理）。
//!
//! `csv` クレートは追加せず、RFC4180 準拠のクォート処理を自前で持つ
//! （`core/Cargo.toml` の依存最小方針。`platform/windows.rs` の `quote_arg`
//! と同じ「純粋関数＋充実した単体テスト」の考え方に揃える）。

use crate::breakdown::{self, BucketBreakdown, CategoryBreakdown};
use crate::delete::{DeleteAction, DeleteMethod, DeletePlan, DryRunReason, ExclusionReason};
use crate::entry::ScanEntry;
use crate::rule::{Rule, Safety};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// エクスポートするJSONのスキーマバージョン。フィールドの追加は加算的な
/// 変更として扱い上げないが、既存フィールドの意味やCSVの列順を変える場合は
/// 上げること（自動化連携という Issue の目的上、破壊的変更を検知できる
/// 必要があるため）。
pub const SCHEMA_VERSION: u32 = 1;

/// JSON シリアライズに失敗した場合のエラー。`serde_json::Error` の内部表現を
/// `core` の公開 API に漏らさない（`PlatformError::Trash` と同じ方針）。
#[derive(Debug)]
pub struct ExportError(String);

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "エクスポートの生成に失敗しました: {}", self.0)
    }
}

impl std::error::Error for ExportError {}

impl From<serde_json::Error> for ExportError {
    fn from(e: serde_json::Error) -> Self {
        ExportError(e.to_string())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rule_index(rules: &[Rule]) -> HashMap<&str, &Rule> {
    rules.iter().map(|r| (r.id.as_str(), r)).collect()
}

/// `rule_id` からラベルと安全度を引く。未知のルール（許可リストに無い等）は
/// ラベルを `rule_id` そのもので代用し、安全度は最も注意深い区分
/// （[`Safety::Review`]）にフォールバックする。エクスポートは表示専用で
/// 削除の可否には影響しないが、「不明なものは安全側に倒す」という
/// NF-SAF-01 の精神を表示上も踏襲する。
fn rule_lookup(index: &HashMap<&str, &Rule>, rule_id: &str) -> (String, Safety) {
    match index.get(rule_id) {
        Some(rule) => (rule.label.clone(), rule.safety),
        None => (rule_id.to_string(), Safety::Review),
    }
}

// ---------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------

#[derive(Serialize)]
struct ScanItemExport {
    rule_id: String,
    rule_label: String,
    safety: Safety,
    path: PathBuf,
    size: u64,
    file_count: u64,
    age_days: Option<u64>,
    recommended: bool,
    selected: bool,
    reason: String,
}

#[derive(Serialize)]
struct ScanSummary {
    item_count: usize,
    selected_count: usize,
    total_size: u64,
    selected_size: u64,
}

#[derive(Serialize)]
struct ScanExport {
    schema_version: u32,
    tool_version: &'static str,
    generated_at_secs: u64,
    kind: &'static str,
    summary: ScanSummary,
    items: Vec<ScanItemExport>,
    by_rule: Vec<CategoryBreakdown>,
    by_size_bucket: Vec<BucketBreakdown>,
}

/// 走査結果（`pc-cleaner scan` の一覧）を JSON へエクスポートする
/// （F-CLI-09）。
pub fn scan_to_json(entries: &[ScanEntry], rules: &[Rule]) -> Result<String, ExportError> {
    let index = rule_index(rules);
    let items: Vec<ScanItemExport> = entries
        .iter()
        .map(|e| {
            let (rule_label, safety) = rule_lookup(&index, &e.rule_id);
            ScanItemExport {
                rule_id: e.rule_id.clone(),
                rule_label,
                safety,
                path: e.path.clone(),
                size: e.size,
                file_count: e.file_count,
                age_days: e.age_days,
                recommended: e.recommended,
                selected: e.selected,
                reason: e.reason.clone(),
            }
        })
        .collect();

    let selected_count = entries.iter().filter(|e| e.selected).count();
    let selected_size: u64 = entries.iter().filter(|e| e.selected).map(|e| e.size).sum();
    let total_size: u64 = entries.iter().map(|e| e.size).sum();
    let breakdown_items = breakdown::from_selected_entries(entries);

    let export = ScanExport {
        schema_version: SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION"),
        generated_at_secs: now_secs(),
        kind: "scan",
        summary: ScanSummary {
            item_count: entries.len(),
            selected_count,
            total_size,
            selected_size,
        },
        items,
        by_rule: breakdown::by_rule(&breakdown_items),
        by_size_bucket: breakdown::by_size_bucket(&breakdown_items),
    };
    Ok(serde_json::to_string_pretty(&export)?)
}

#[derive(Serialize)]
struct PlanItemExport {
    rule_id: String,
    rule_label: String,
    safety: Safety,
    path: PathBuf,
    size: u64,
    file_count: u64,
    action: DeleteAction,
}

#[derive(Serialize)]
struct ExcludedItemExport {
    rule_id: String,
    path: PathBuf,
    size: u64,
    reason: ExclusionReason,
}

#[derive(Serialize)]
struct PlanSummary {
    item_count: usize,
    total_size: u64,
    total_file_count: u64,
}

#[derive(Serialize)]
struct PlanExport {
    schema_version: u32,
    tool_version: &'static str,
    generated_at_secs: u64,
    kind: &'static str,
    dry_run: bool,
    dry_run_reason: Option<DryRunReason>,
    method: DeleteMethod,
    summary: PlanSummary,
    items: Vec<PlanItemExport>,
    excluded: Vec<ExcludedItemExport>,
    by_rule: Vec<CategoryBreakdown>,
    by_size_bucket: Vec<BucketBreakdown>,
}

/// 削除計画（`pc-cleaner clean --dry-run` のプレビュー）を JSON へエクス
/// ポートする（F-CLI-09）。`plan` は不変参照で受け取るだけで消費しない
/// （モジュール doc 参照）。
pub fn plan_to_json(plan: &DeletePlan, rules: &[Rule]) -> Result<String, ExportError> {
    let index = rule_index(rules);
    let items: Vec<PlanItemExport> = plan
        .items()
        .iter()
        .map(|item| {
            let (rule_label, safety) = rule_lookup(&index, &item.rule_id);
            PlanItemExport {
                rule_id: item.rule_id.clone(),
                rule_label,
                safety,
                path: item.path.clone(),
                size: item.size,
                file_count: item.file_count,
                action: item.action,
            }
        })
        .collect();
    let excluded: Vec<ExcludedItemExport> = plan
        .excluded()
        .iter()
        .map(|e| ExcludedItemExport {
            rule_id: e.rule_id.clone(),
            path: e.path.clone(),
            size: e.size,
            reason: e.reason,
        })
        .collect();
    let breakdown_items = breakdown::from_plan(plan);

    let export = PlanExport {
        schema_version: SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION"),
        generated_at_secs: now_secs(),
        kind: "plan",
        dry_run: plan.mode().is_dry_run(),
        dry_run_reason: plan.mode().dry_run_reason(),
        method: plan.mode().method(),
        summary: PlanSummary {
            item_count: plan.item_count(),
            total_size: plan.total_size(),
            total_file_count: plan.total_file_count(),
        },
        items,
        excluded,
        by_rule: breakdown::by_rule(&breakdown_items),
        by_size_bucket: breakdown::by_size_bucket(&breakdown_items),
    };
    Ok(serde_json::to_string_pretty(&export)?)
}

// ---------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------

/// scan/plan で共有する CSV のヘッダ行。`scan` では `action` が常に空、
/// `plan` では `age_days` / `recommended` / `selected` が常に空になる
/// （項目の性質上、両方を同時に持つエントリが無いため）。
const CSV_HEADER: &str = "kind,rule_id,rule_label,safety,path,size_bytes,file_count,age_days,recommended,selected,action,reason";

/// RFC4180 準拠のフィールドクォート。カンマ・ダブルクォート・改行
/// （`\n` / `\r`）を含む場合のみ全体を `"` で囲み、内部の `"` を `""` に
/// 二重化する。それ以外はそのまま返す。
pub(crate) fn quote_csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn write_csv_row(out: &mut String, fields: &[String]) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&quote_csv_field(field));
    }
    out.push('\n');
}

/// 走査結果を CSV へエクスポートする（F-CLI-09）。ヘッダ行 + 1項目1行。
pub fn scan_to_csv(entries: &[ScanEntry], rules: &[Rule]) -> String {
    let index = rule_index(rules);
    let mut out = String::new();
    out.push_str(CSV_HEADER);
    out.push('\n');
    for e in entries {
        let (rule_label, safety) = rule_lookup(&index, &e.rule_id);
        write_csv_row(
            &mut out,
            &[
                "scan".to_string(),
                e.rule_id.clone(),
                rule_label,
                format!("{safety:?}"),
                e.path.display().to_string(),
                e.size.to_string(),
                e.file_count.to_string(),
                e.age_days.map(|d| d.to_string()).unwrap_or_default(),
                e.recommended.to_string(),
                e.selected.to_string(),
                String::new(),
                e.reason.clone(),
            ],
        );
    }
    out
}

/// 削除計画を CSV へエクスポートする（F-CLI-09）。ヘッダ行 + 1項目1行。
/// 除外項目（`excluded`）も `kind = "excluded"` として含める（「なぜ消え
/// ないのか」のレビューに使えるようにするため）。`plan` は不変参照で
/// 受け取るだけで消費しない（モジュール doc 参照）。
pub fn plan_to_csv(plan: &DeletePlan, rules: &[Rule]) -> String {
    let index = rule_index(rules);
    let mut out = String::new();
    out.push_str(CSV_HEADER);
    out.push('\n');
    for item in plan.items() {
        let (rule_label, safety) = rule_lookup(&index, &item.rule_id);
        write_csv_row(
            &mut out,
            &[
                "plan".to_string(),
                item.rule_id.clone(),
                rule_label,
                format!("{safety:?}"),
                item.path.display().to_string(),
                item.size.to_string(),
                item.file_count.to_string(),
                String::new(),
                String::new(),
                String::new(),
                format!("{:?}", item.action),
                String::new(),
            ],
        );
    }
    for e in plan.excluded() {
        let (rule_label, safety) = rule_lookup(&index, &e.rule_id);
        write_csv_row(
            &mut out,
            &[
                "excluded".to_string(),
                e.rule_id.clone(),
                rule_label,
                format!("{safety:?}"),
                e.path.display().to_string(),
                e.size.to_string(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                format!("{:?}", e.reason),
            ],
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::delete::{DeleteMode, DeleteRequest, preview};
    use crate::platform::{ElevateError, ElevateResult, KnownDir, Platform};
    use crate::rule::MatchKind;
    use serde_json::Value;
    use std::path::Path;

    // ---- quote_csv_field ----

    #[test]
    fn quote_csv_field_passes_through_plain_values() {
        assert_eq!(quote_csv_field(""), "");
        assert_eq!(quote_csv_field("user_temp"), "user_temp");
        assert_eq!(
            quote_csv_field(r"C:\Users\alice\Temp"),
            r"C:\Users\alice\Temp"
        );
    }

    #[test]
    fn quote_csv_field_wraps_values_with_commas() {
        assert_eq!(quote_csv_field("a,b"), "\"a,b\"");
    }

    #[test]
    fn quote_csv_field_escapes_embedded_quotes() {
        assert_eq!(quote_csv_field(r#"say "hi""#), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn quote_csv_field_wraps_values_with_newlines() {
        assert_eq!(quote_csv_field("a\nb"), "\"a\nb\"");
        assert_eq!(quote_csv_field("a\rb"), "\"a\rb\"");
    }

    // ---- テスト用の最小データ ----

    fn sample_rule() -> Rule {
        Rule {
            id: "user_temp".to_string(),
            label: "ユーザー一時ファイル".to_string(),
            description: "テスト用".to_string(),
            base: KnownDir::UserTemp,
            match_kind: MatchKind::All,
            needs_admin: false,
            safety: Safety::Safe,
            age_threshold_days: None,
            large_file_threshold_bytes: None,
            is_user_defined: false,
        }
    }

    fn sample_entry(path: &str, size: u64, selected: bool) -> ScanEntry {
        ScanEntry {
            rule_id: "user_temp".to_string(),
            path: PathBuf::from(path),
            size,
            file_count: 1,
            modified: None,
            age_days: Some(3),
            in_use: None,
            duplicate: None,
            recommended: true,
            reason: "180日以上経過".to_string(),
            selected,
        }
    }

    struct FakePlatform {
        temp_dir: PathBuf,
    }

    impl Platform for FakePlatform {
        fn known_dir(&self, kind: KnownDir) -> Option<PathBuf> {
            match kind {
                KnownDir::UserTemp => Some(self.temp_dir.clone()),
                _ => None,
            }
        }
        fn to_trash(&self, _path: &Path) -> crate::platform::Result<()> {
            Ok(())
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
    }

    fn sample_plan() -> DeletePlan {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let file = base.join("a.tmp");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&file, b"hello").unwrap();
        let platform = FakePlatform {
            temp_dir: base.clone(),
        };
        let entries = vec![sample_entry(file.to_str().unwrap(), 5, true)];
        let mode = DeleteMode::resolve(&Config::default(), DeleteRequest::dry_run());
        preview(&platform, &entries, &[sample_rule()], mode)
    }

    // ---- JSON ----

    #[test]
    fn scan_to_json_summary_matches_entries() {
        let entries = vec![
            sample_entry("a.tmp", 10, true),
            sample_entry("b.tmp", 20, false),
        ];
        let json = scan_to_json(&entries, &[sample_rule()]).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["kind"], "scan");
        assert_eq!(value["summary"]["item_count"], 2);
        assert_eq!(value["summary"]["selected_count"], 1);
        assert_eq!(value["summary"]["total_size"], 30);
        assert_eq!(value["summary"]["selected_size"], 10);
        assert_eq!(value["items"][0]["rule_label"], "ユーザー一時ファイル");
        assert_eq!(value["items"][0]["safety"], "Safe");
    }

    #[test]
    fn scan_to_json_falls_back_to_rule_id_for_unknown_rule() {
        let entries = vec![sample_entry("a.tmp", 10, true)];
        let json = scan_to_json(&entries, &[]).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["items"][0]["rule_label"], "user_temp");
        assert_eq!(value["items"][0]["safety"], "Review");
    }

    #[test]
    fn plan_to_json_summary_matches_plan_totals() {
        let plan = sample_plan();
        let json = plan_to_json(&plan, &[sample_rule()]).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["kind"], "plan");
        assert!(value["dry_run"].as_bool().unwrap());
        assert_eq!(value["summary"]["item_count"], plan.item_count());
        assert_eq!(value["summary"]["total_size"], plan.total_size());
        assert_eq!(
            value["summary"]["total_file_count"],
            plan.total_file_count()
        );
        assert_eq!(value["items"][0]["action"], "ToTrash");
    }

    #[test]
    fn plan_to_json_breakdown_total_matches_plan_total_size() {
        // delete.rs の breakdown_from_plan_matches_total_size と同じ趣旨：
        // エクスポートの内訳合計が本体の集計とズレないことの退行テスト。
        let plan = sample_plan();
        let json = plan_to_json(&plan, &[sample_rule()]).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        let breakdown_total: u64 = value["by_rule"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["total_size"].as_u64().unwrap())
            .sum();
        assert_eq!(breakdown_total, plan.total_size());
    }

    // ---- CSV ----

    #[test]
    fn scan_to_csv_has_header_and_one_row_per_entry() {
        let entries = vec![
            sample_entry("a.tmp", 10, true),
            sample_entry("b.tmp", 20, false),
        ];
        let csv = scan_to_csv(&entries, &[sample_rule()]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3, "ヘッダ + 2行");
        assert_eq!(lines[0], CSV_HEADER);
        assert!(
            lines[1].contains("scan,user_temp,ユーザー一時ファイル,Safe,a.tmp,10,1,3,true,true,,")
        );
    }

    #[test]
    fn plan_to_csv_row_count_matches_items_plus_excluded() {
        let plan = sample_plan();
        let csv = plan_to_csv(&plan, &[sample_rule()]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines.len(),
            1 + plan.item_count() + plan.excluded().len(),
            "ヘッダ + items + excluded"
        );
        assert_eq!(lines[0], CSV_HEADER);
    }

    #[test]
    fn csv_quotes_paths_containing_commas() {
        let entries = vec![sample_entry("a,b.tmp", 10, true)];
        let csv = scan_to_csv(&entries, &[sample_rule()]);
        assert!(csv.contains("\"a,b.tmp\""));
    }
}
