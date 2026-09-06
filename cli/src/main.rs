//! pc-cleaner の薄い CLI（F-CLI-01〜08）。
//!
//! 判定・削除ロジックは一切持たず、`pc-cleaner-core` の呼び出しと表示のみを
//! 行う（F-CLI-01 / NF-MNT-01）。CLI で固めた挙動がそのまま GUI に乗る
//! （設計目標 G4 / F-CLI-08）。
//!
//! ## 終了コード
//!
//! | コード | 意味 |
//! |---|---|
//! | `0` | 正常終了。`--admin` により昇格して再実行を開始した場合も含む
//! |     | （元プロセスはその時点で正常に役目を終えたとみなす） |
//! | `1` | 削除に失敗した項目がある（既存）、または昇格自体に失敗した |
//! | `2` | `--admin` 指定時、UAC の確認画面でユーザーがキャンセルした |
//! |     |（自動化から「同意が得られなかった」を検出できるようにするため） |

#![deny(unsafe_code)]

use clap::{Args, Parser, Subcommand, ValueEnum};
use pc_cleaner_core::{
    AuditRecord, Config, DeleteMode, DeleteOutcome, DeletePlan, DeleteRequest, ElevateError,
    ItemOutcome, RestoreCandidate, RestoreItemOutcome, RuleSet, ScanEntry, audit, breakdown,
    config, execute, export, history, human_size, platform, preview, restore, safety_scope,
    scan_pipeline, should_relaunch,
};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

/// `scan` / `clean` の出力形式（D2 / Issue #52）。`Text`（既定）は従来通りの
/// 人間向け一覧を stdout に出す。`Json` / `Csv` は走査結果・削除計画を機械
/// 可読な形でエクスポートする（削除の挙動そのものは変えない、F-CLI-09）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    Csv,
}

#[derive(Parser)]
#[command(name = "pc-cleaner", about = "手動選択型ディスク掃除ツール", version)]
struct Cli {
    /// 内部用。管理者権限で起動し直された後のプロセスであることを示す
    /// マーカー。再昇格ループを防ぐためだけに使い、利用者が指定するもの
    /// ではない（A2 / Issue #41）。
    #[arg(long, hide = true, global = true)]
    elevated: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 走査結果を一覧表示する（削除は行わない、F-CLI-02）
    Scan(ScopeArgs),
    /// 走査して削除を実行する（既定はゴミ箱送り、F-CLI-03〜06）
    Clean(CleanArgs),
    /// 削除ログ（監査ログ）を表示する（D1 / Issue #51）。何も削除しない
    Log(LogArgs),
    /// ゴミ箱からの復元を補助する（D3 / Issue #53）。既定では一覧表示のみ
    Restore(RestoreArgs),
}

#[derive(Args)]
struct RestoreArgs {
    /// 指定した実行（`log` で確認できる run_id）分のみを対象にする
    #[arg(long)]
    run: Option<u64>,
    /// 指定したパス（元の場所、完全一致）のみを対象にする
    #[arg(long)]
    path: Option<PathBuf>,
    /// 削除ログと突き合わせられない項目（他アプリが削除した可能性がある）
    /// も一覧・対象に含める（既定では pc-cleaner が削除したと確認できた
    /// ものだけを対象にする）
    #[arg(long)]
    all_trash: bool,
    /// 一覧表示だけでなく実際に復元する（省略時は一覧表示のみで何も
    /// 変更しない。`clean --dry-run` と同じ「明示しない限り状態を変えない」
    /// 方針）
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct LogArgs {
    /// 表示件数の上限
    #[arg(long, default_value_t = 20)]
    limit: usize,
    /// 指定した実行（`clean` 実行時に払い出される run_id）分のみ表示する
    #[arg(long)]
    run: Option<u64>,
}

#[derive(Args)]
struct ScopeArgs {
    /// Caution / Review ルールも対象に含める（既定は Safe のみ）
    #[arg(long)]
    all: bool,
    /// 管理者権限が必要な領域も対象にする。未昇格の場合は UAC の確認を経て
    /// 管理者として起動し直し、現在のプロセスは終了する（A2 / Issue #41）。
    #[arg(long)]
    admin: bool,
    /// 出力形式（D2 / Issue #52）。`json` / `csv` を指定すると、走査結果を
    /// 機械可読な形でエクスポートする（削除は行わない、F-CLI-02 は不変）。
    #[arg(long, value_enum, default_value = "text")]
    format: OutputFormat,
    /// エクスポート先ファイル。省略時は stdout に出力する
    /// （`--format text` のときは無視される）。
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Args)]
struct CleanArgs {
    /// Caution / Review ルールも対象に含める（既定は Safe のみ）
    #[arg(long)]
    all: bool,
    /// 削除を行わず、削除予定のプレビューのみ表示する（F-CLI-03）
    #[arg(long)]
    dry_run: bool,
    /// ゴミ箱を経由せず完全削除する。復旧不可のため確認を要する（F-CLI-05 / F-DEL-06）
    #[arg(long)]
    permanent: bool,
    /// 完全削除の確認プロンプトを省略する（自動化用途。`--permanent` と併用時のみ意味を持つ）
    #[arg(long)]
    yes: bool,
    /// 管理者権限が必要な領域も対象にする。未昇格の場合は UAC の確認を経て
    /// 管理者として起動し直し、現在のプロセスは終了する（A2 / Issue #41）。
    #[arg(long)]
    admin: bool,
    /// 出力形式（D2 / Issue #52）。`json` / `csv` を指定すると、削除計画
    /// （プレビュー）を機械可読な形でエクスポートする。削除の挙動そのもの
    /// は変えない：`--format json` 単体で `clean` を実行すれば通常どおり
    /// 削除が実行される（F-CLI-09）。
    #[arg(long, value_enum, default_value = "text")]
    format: OutputFormat,
    /// エクスポート先ファイル。省略時は stdout に出力する
    /// （`--format text` のときは無視される）。
    #[arg(long)]
    output: Option<PathBuf>,
}

impl Command {
    /// `--admin` が指定されたか（Scan/Clean 共通）。
    fn admin_requested(&self) -> bool {
        match self {
            Command::Scan(args) => args.admin,
            Command::Clean(args) => args.admin,
            Command::Log(_) => false,
            Command::Restore(_) => false,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let platform = platform::current();

    if should_relaunch(
        cli.command.admin_requested(),
        platform.is_elevated(),
        cli.elevated,
    ) {
        return run_elevate(platform.as_ref());
    }
    if cli.command.admin_requested() && cli.elevated && !platform.is_elevated() {
        // 二重防御（should_relaunch のコメント参照）が効いた場合。通常は
        // 起こらないが、起きた場合は管理者領域を対象外のまま静かに続行する
        // のではなく、利用者に理由を伝える。
        eprintln!(
            "管理者権限で起動し直しましたが、昇格を確認できませんでした。\
             管理者権限が必要な領域は対象外のまま続行します。"
        );
    }

    let config = load_config(platform.as_ref());

    match cli.command {
        Command::Scan(args) => run_scan(platform.as_ref(), &config, args),
        Command::Clean(args) => run_clean(platform.as_ref(), &config, args),
        Command::Log(args) => run_log(platform.as_ref(), args),
        Command::Restore(args) => run_restore(platform.as_ref(), args),
    }
}

/// 昇格後プロセスへ渡す引数列を作る。元の引数（`argv[0]` を除く）をそのまま
/// 引き継ぎ、末尾に内部用マーカー `--elevated` を足す。
fn relaunch_args(argv_rest: &[String]) -> Vec<String> {
    let mut args = argv_rest.to_vec();
    args.push("--elevated".to_string());
    args
}

/// `--admin` により管理者権限へ昇格して自プロセスを起動し直す
/// （F-ELV-01 / A2 / Issue #41）。
///
/// **注意**：本 PR（A2）の時点では、昇格しても走査・削除の対象は増えない。
/// `needs_admin` なルールを実際に対象化するのは A1（Issue #40、次段の PR）
/// の責務であり、ここでは「昇格の仕組み」だけを提供する。
fn run_elevate(platform: &dyn platform::Platform) -> ExitCode {
    let argv_rest: Vec<String> = std::env::args().skip(1).collect();
    let args = relaunch_args(&argv_rest);

    println!("管理者権限が必要な領域を対象にするため、管理者として実行し直します。");
    println!("UAC の確認画面で「はい」を選択してください。");

    match platform.elevate(&args) {
        Ok(()) => {
            println!("管理者権限で開き直しました。このウィンドウは終了します。");
            println!("結果は新しいウィンドウに表示されますが、完了と同時に閉じます。");
            println!(
                "出力を確認したい場合は、管理者としてターミナルを開いてから \
                 pc-cleaner を実行してください（この場合 --admin は不要です）。"
            );
            ExitCode::SUCCESS
        }
        Err(ElevateError::Cancelled) => {
            eprintln!("管理者権限での実行はキャンセルされました。");
            eprintln!("--admin を外せば、管理者権限が不要な領域だけを対象に実行できます。");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("管理者権限で実行し直せませんでした: {e}");
            ExitCode::FAILURE
        }
    }
}

fn load_config(platform: &dyn platform::Platform) -> Config {
    match config::config_file_path(platform) {
        Some(path) => config::load(&path),
        None => Config::default(),
    }
}

/// core の [`scan_pipeline`] を呼ぶだけの薄いラッパー。GUI（`gui/src/task.rs`
/// の走査ワーカー）と同じ core 呼び出し列であることを維持すること（F-CLI-08）。
///
/// ルール一覧は [`RuleSet::load`] で解決する（組み込み + ルール定義ファイル
/// の上書き・追加、D4 / Issue #54）。ファイルの検証で問題が見つかった場合は
/// 警告として表示するが、走査自体は組み込みルールのみで続行する（F-CLI-01：
/// CLI は実行そのものを妨げない）。
fn scan_and_apply_prefs(
    platform: &dyn platform::Platform,
    config: &Config,
    all: bool,
) -> (Vec<pc_cleaner_core::Rule>, Vec<ScanEntry>) {
    let rule_set = RuleSet::load(platform, config);
    for issue in rule_set.issues() {
        eprintln!("警告（ルール定義）: {}", issue.message);
    }
    let rules = rule_set.for_safeties(&safety_scope(all), platform.is_elevated());
    let entries = scan_pipeline(platform, &rules, config);
    (rules, entries)
}

fn run_scan(platform: &dyn platform::Platform, config: &Config, args: ScopeArgs) -> ExitCode {
    let (rules, entries) = scan_and_apply_prefs(platform, config, args.all);

    match args.format {
        OutputFormat::Text => {
            print_scan_report(&entries);
            ExitCode::SUCCESS
        }
        OutputFormat::Json => match export::scan_to_json(&entries, &rules) {
            Ok(data) => emit_export(&data, args.output.as_deref(), entries.len()),
            Err(e) => {
                eprintln!("エクスポートに失敗しました: {e}");
                ExitCode::FAILURE
            }
        },
        OutputFormat::Csv => {
            let data = export::scan_to_csv(&entries, &rules);
            emit_export(&data, args.output.as_deref(), entries.len())
        }
    }
}

fn run_clean(platform: &dyn platform::Platform, config: &Config, args: CleanArgs) -> ExitCode {
    let (rules, entries) = scan_and_apply_prefs(platform, config, args.all);

    // 完全削除は、確認が取れるまで「未確定」（＝プレビューのみ）として扱う。
    let initial_request = match (args.dry_run, args.permanent) {
        (_, true) => DeleteRequest::permanent_unconfirmed(),
        (true, false) => DeleteRequest::dry_run(),
        (false, false) => DeleteRequest::execute(),
    };

    let mode = DeleteMode::resolve(config, initial_request);
    let plan = preview(platform, &entries, &rules, mode);
    if args.format == OutputFormat::Text {
        print_plan_summary(&plan);
    }

    let final_plan = if args.permanent && !args.dry_run && !plan.is_empty() {
        let confirmed = if args.yes {
            true
        } else if args.format == OutputFormat::Text {
            confirm_permanent(plan.item_count(), plan.total_size())
        } else {
            // 機械可読な出力を要求されている場合、対話プロンプトは自動化を
            // 壊すため出さない。安全側に倒し、確認なしでは完全削除を実行
            // しない（F-DEL-06）。
            eprintln!(
                "--permanent は --format text 以外では --yes と併用してください（対話確認は省略されました）。"
            );
            false
        };
        if confirmed {
            let confirmed_mode = DeleteMode::resolve(config, DeleteRequest::permanent_confirmed());
            preview(platform, &entries, &rules, confirmed_mode)
        } else {
            if args.format == OutputFormat::Text {
                println!("キャンセルしました。");
            }
            plan
        }
    } else {
        plan
    };

    // final_plan は execute() に値で渡すと消費されるため、エクスポートは
    // その前に行う（`export.rs` は不変参照しか取らず、消費しない）。
    if args.format != OutputFormat::Text {
        let export_result = match args.format {
            OutputFormat::Json => {
                export::plan_to_json(&final_plan, &rules).map_err(|e| e.to_string())
            }
            OutputFormat::Csv => Ok(export::plan_to_csv(&final_plan, &rules)),
            OutputFormat::Text => unreachable!(),
        };
        match export_result {
            Ok(data) => {
                let code = emit_export(&data, args.output.as_deref(), final_plan.item_count());
                if code != ExitCode::SUCCESS {
                    return code;
                }
            }
            Err(e) => {
                eprintln!("エクスポートに失敗しました: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let outcome = execute(platform, final_plan);
    if args.format == OutputFormat::Text {
        print_outcome(&outcome);
    }
    if !outcome.is_dry_run() {
        let run_id = audit::next_run_id(SystemTime::now());
        record_history(platform, &outcome, run_id);
        record_audit(platform, config, &outcome, run_id);
    }

    if outcome.failed_count() > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn confirm_permanent(item_count: usize, total_size: u64) -> bool {
    print!(
        "{item_count} 件（{}）を完全に削除します。ゴミ箱を経由せず復元できません。続行しますか？ [y/N]: ",
        human_size(total_size)
    );
    if io::stdout().flush().is_err() {
        return false;
    }
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    is_affirmative(&input)
}

/// エクスポートデータ（JSON/CSV）を `output`（指定時はファイル、未指定なら
/// stdout）へ書き出す（D2 / Issue #52）。`--format json|csv` かつ `--output`
/// 未指定のときは、機械可読データだけを stdout に出す必要があるため、詳細な
/// 一覧・サマリ（`print_scan_report` / `print_plan_summary` 等）は呼び出し
/// 側で呼ばない（自動化のパイプにテキストが混ざるのを防ぐ、F-CLI-10）。
fn emit_export(data: &str, output: Option<&Path>, item_count: usize) -> ExitCode {
    match output {
        Some(path) => match fs::write(path, data) {
            Ok(()) => {
                println!("{item_count} 件を書き出しました: {}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("書き出しに失敗しました（{}）: {e}", path.display());
                ExitCode::FAILURE
            }
        },
        None => {
            print!("{data}");
            if !data.ends_with('\n') {
                println!();
            }
            ExitCode::SUCCESS
        }
    }
}

fn is_affirmative(input: &str) -> bool {
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

// ---------------------------------------------------------------------
// 出力
// ---------------------------------------------------------------------

/// 走査結果を一覧表示する。ルール・パス・サイズ・経過日数・推奨可否・
/// 合計解放見込みを含める（F-CLI-07）。
fn print_scan_report(entries: &[ScanEntry]) {
    if entries.is_empty() {
        println!("対象なし。");
        return;
    }

    for entry in entries {
        let mark = if entry.selected { "[x]" } else { "[ ]" };
        let age = entry
            .age_days
            .map(|d| format!("{d}日"))
            .unwrap_or_else(|| "不明".to_string());
        let recommended = if entry.recommended { "推奨" } else { "-" };
        println!(
            "{mark} {:<16} {:>10}  経過:{age:<5} {recommended:<4} {}",
            entry.rule_id,
            human_size(entry.size),
            entry.path.display(),
        );
        if !entry.reason.is_empty() {
            println!("      理由: {}", entry.reason);
        }
    }

    let selected_count = entries.iter().filter(|e| e.selected).count();
    let selected_size: u64 = entries.iter().filter(|e| e.selected).map(|e| e.size).sum();
    println!();
    println!(
        "{} 件中 {selected_count} 件を選択中（解放見込み: {}）",
        entries.len(),
        human_size(selected_size)
    );
}

/// 削除計画のプレビューを表示する（F-CLI-03 / F-DEL-04）。
fn print_plan_summary(plan: &DeletePlan) {
    println!(
        "削除予定: {} 件（{}）{}",
        plan.item_count(),
        human_size(plan.total_size()),
        if plan.mode().is_dry_run() {
            "［ドライラン］"
        } else {
            ""
        }
    );
    // 種類別の内訳（C3 / GUI と同じ集計ロジックを core で共有する）。
    let by_rule = breakdown::by_rule(&breakdown::from_plan(plan));
    if !by_rule.is_empty() {
        println!("内訳（種類別）:");
        for category in &by_rule {
            println!(
                "  {:<16} {} 件 / {}",
                category.rule_id,
                category.item_count,
                human_size(category.total_size)
            );
        }
    }
    for item in plan.items() {
        println!("  {:<16} {}", item.rule_id, item.path.display());
    }
    if !plan.excluded().is_empty() {
        println!(
            "除外: {} 件（選択されていない・許可リスト外・要管理者権限など）",
            plan.excluded().len()
        );
    }
}

/// 削除の実行結果を表示する。
fn print_outcome(outcome: &DeleteOutcome) {
    if outcome.is_dry_run() {
        println!("ドライランのため削除は行われていません。");
        return;
    }

    println!(
        "削除完了: 成功 {} 件、失敗 {} 件、解放 {}",
        outcome.deleted_count(),
        outcome.failed_count(),
        human_size(outcome.freed_bytes())
    );
    for result in &outcome.results {
        if let ItemOutcome::Failed { message } = &result.outcome {
            eprintln!("  失敗: {} — {message}", result.path.display());
        }
    }
}

/// `pc-cleaner log` の実装。監査ログ（`deletion_log.jsonl`）を読んで表示する
/// だけで、削除・復元は一切行わない（D1 / Issue #51）。
fn run_log(platform: &dyn platform::Platform, args: LogArgs) -> ExitCode {
    let Some(path) = audit::audit_file_path(platform) else {
        eprintln!("この環境では削除ログの保存先を特定できません。");
        return ExitCode::FAILURE;
    };

    let records = match audit::load(&path) {
        Ok(records) => records,
        Err(e) => {
            eprintln!("削除ログの読み込みに失敗しました: {e}");
            return ExitCode::FAILURE;
        }
    };

    let filtered: Vec<&AuditRecord> = match args.run {
        Some(run_id) => audit::filter_by_run(&records, run_id),
        None => records.iter().collect(),
    };

    if filtered.is_empty() {
        println!("記録なし。");
        return ExitCode::SUCCESS;
    }

    let start = filtered.len().saturating_sub(args.limit);
    for record in &filtered[start..] {
        print_audit_record(record);
    }
    println!("保存先: {}", path.display());
    ExitCode::SUCCESS
}

fn print_audit_record(record: &AuditRecord) {
    let outcome = match &record.outcome {
        ItemOutcome::Deleted => "削除".to_string(),
        ItemOutcome::Failed { message } => format!("失敗（{message}）"),
        ItemOutcome::Missing => "対象なし".to_string(),
        ItemOutcome::NotAttempted => "未実行".to_string(),
    };
    println!(
        "[run {}] {:<16} {:>10}  {:<10} {}",
        record.run_id,
        record.rule_id,
        human_size(record.size),
        outcome,
        record.path.display(),
    );
}

/// `pc-cleaner restore` の実装（D3 / Issue #53）。
///
/// 既定では一覧表示のみで、`--yes` を明示したときだけ実際に復元する
/// （`clean` が既定ドライランであるのと同じ「明示しない限り状態を変えない」
/// 方針）。復元は削除ではないため許可リスト方式（NF-SAF-04）の対象外だが、
/// 復元先に既存ファイルがあれば自動上書きしない（NF-SAF-01。
/// `Platform::restore_from_trash` のドキュメント参照）。
fn run_restore(platform: &dyn platform::Platform, args: RestoreArgs) -> ExitCode {
    let trash = match platform.list_trash() {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("ゴミ箱の一覧を取得できませんでした: {e}");
            return ExitCode::FAILURE;
        }
    };

    let audit_records = match audit::audit_file_path(platform) {
        Some(path) => match audit::load(&path) {
            Ok(records) => records,
            Err(e) => {
                eprintln!("警告: 削除ログの読み込みに失敗しました: {e}（由来不明として扱います）");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    let mut candidates = restore::correlate(&trash, &audit_records);
    if !args.all_trash {
        candidates.retain(|c| c.rule_id.is_some());
    }
    if let Some(run_id) = args.run {
        candidates.retain(|c| c.run_id == Some(run_id));
    }
    if let Some(path) = &args.path {
        candidates.retain(|c| &c.entry.original_path == path);
    }

    if candidates.is_empty() {
        println!("復元候補なし。");
        return ExitCode::SUCCESS;
    }

    if !args.yes {
        println!(
            "復元候補: {} 件（--yes を付けると実際に復元します。何も変更していません）",
            candidates.len()
        );
        for candidate in &candidates {
            print_restore_candidate(candidate);
        }
        return ExitCode::SUCCESS;
    }

    let outcome = restore::restore(platform, &candidates);
    println!(
        "復元完了: 成功 {} 件、衝突によるスキップ {} 件、対象消失 {} 件、失敗 {} 件",
        outcome.restored_count(),
        outcome.skipped_count(),
        outcome.not_found_count(),
        outcome.failed_count()
    );
    for (path, item_outcome) in &outcome.results {
        if let RestoreItemOutcome::Failed { message } = item_outcome {
            eprintln!("  失敗: {} — {message}", path.display());
        }
    }

    if outcome.failed_count() > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_restore_candidate(candidate: &RestoreCandidate) {
    let origin = match (&candidate.rule_id, candidate.run_id) {
        (Some(rule_id), Some(run_id)) => format!("[run {run_id}] {rule_id}"),
        _ => "由来不明".to_string(),
    };
    let size = candidate
        .entry
        .size
        .map(human_size)
        .unwrap_or_else(|| "?".to_string());
    println!(
        "{origin:<24} {size:>10}  {}",
        candidate.entry.original_path.display()
    );
}

/// 削除結果を履歴（`history.json`）へ記録する（C4 / Issue #50）。
///
/// 履歴の記録は削除の成否そのものには影響させない。書き込みに失敗しても
/// 警告を出すだけで、`run_clean` 全体の終了コードは変えない（F-CLI-01：
/// CLI は薄く保ち、実行そのものを妨げない）。
fn record_history(platform: &dyn platform::Platform, outcome: &DeleteOutcome, run_id: u64) {
    match history::record_outcome(platform, outcome, run_id) {
        None => {}
        Some(Err(e)) => {
            eprintln!("警告: 履歴の記録に失敗しました: {e}");
        }
        Some(Ok(history)) => {
            println!(
                "累計解放: {}（{}回）",
                human_size(history.total_freed_bytes()),
                history.run_count()
            );
        }
    }
}

/// 削除結果を監査ログ（`deletion_log.jsonl`）へ記録する（D1 / Issue #51）。
///
/// `record_history` と同じ方針：記録の成否は `run_clean` 全体の終了コードに
/// 影響させず、失敗は警告表示に留める（F-CLI-01：CLI は実行そのものを
/// 妨げない）。`Config::audit_log_enabled` が `false` の場合は静かに何もしない。
fn record_audit(
    platform: &dyn platform::Platform,
    config: &Config,
    outcome: &DeleteOutcome,
    run_id: u64,
) {
    match audit::record_outcome(platform, config, outcome, run_id) {
        None => {}
        Some(Err(e)) => {
            eprintln!("警告: 削除ログの記録に失敗しました: {e}");
        }
        Some(Ok(_)) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_affirmative_accepts_y_and_yes_case_insensitively() {
        assert!(is_affirmative("y\n"));
        assert!(is_affirmative("Y\n"));
        assert!(is_affirmative("yes\n"));
        assert!(is_affirmative("YES\n"));
        assert!(!is_affirmative("\n"));
        assert!(!is_affirmative("n\n"));
        assert!(!is_affirmative("maybe\n"));
    }

    #[test]
    fn cli_parses_scan_and_clean_subcommands() {
        let cli = Cli::try_parse_from(["pc-cleaner", "scan"]).unwrap();
        assert!(matches!(cli.command, Command::Scan(_)));

        let cli = Cli::try_parse_from(["pc-cleaner", "scan", "--all"]).unwrap();
        match cli.command {
            Command::Scan(args) => assert!(args.all),
            _ => panic!("expected Scan"),
        }

        let cli = Cli::try_parse_from(["pc-cleaner", "clean", "--dry-run", "--permanent", "--yes"])
            .unwrap();
        match cli.command {
            Command::Clean(args) => {
                assert!(args.dry_run);
                assert!(args.permanent);
                assert!(args.yes);
                assert!(!args.all);
                assert!(!args.admin);
                assert_eq!(args.format, OutputFormat::Text);
                assert_eq!(args.output, None);
            }
            _ => panic!("expected Clean"),
        }
    }

    #[test]
    fn cli_parses_format_and_output_on_scan_and_clean() {
        let cli = Cli::try_parse_from([
            "pc-cleaner",
            "scan",
            "--format",
            "json",
            "--output",
            "out.json",
        ])
        .unwrap();
        match cli.command {
            Command::Scan(args) => {
                assert_eq!(args.format, OutputFormat::Json);
                assert_eq!(args.output, Some(PathBuf::from("out.json")));
            }
            _ => panic!("expected Scan"),
        }

        let cli =
            Cli::try_parse_from(["pc-cleaner", "clean", "--dry-run", "--format", "csv"]).unwrap();
        match cli.command {
            Command::Clean(args) => {
                assert_eq!(args.format, OutputFormat::Csv);
                assert_eq!(args.output, None);
            }
            _ => panic!("expected Clean"),
        }
    }

    #[test]
    fn cli_rejects_unknown_format_value() {
        assert!(Cli::try_parse_from(["pc-cleaner", "scan", "--format", "xml"]).is_err());
    }

    #[test]
    fn emit_export_writes_data_to_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        let code = emit_export("{}", Some(path.as_path()), 3);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    }

    #[test]
    fn emit_export_fails_when_output_path_is_invalid() {
        let path = Path::new("/nonexistent-dir-xyz/out.json");
        let code = emit_export("{}", Some(path), 0);
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn cli_parses_log_subcommand_with_defaults_and_options() {
        let cli = Cli::try_parse_from(["pc-cleaner", "log"]).unwrap();
        assert!(!cli.command.admin_requested());
        match cli.command {
            Command::Log(args) => {
                assert_eq!(args.limit, 20);
                assert_eq!(args.run, None);
            }
            _ => panic!("expected Log"),
        }

        let cli =
            Cli::try_parse_from(["pc-cleaner", "log", "--limit", "5", "--run", "42"]).unwrap();
        match cli.command {
            Command::Log(args) => {
                assert_eq!(args.limit, 5);
                assert_eq!(args.run, Some(42));
            }
            _ => panic!("expected Log"),
        }
    }

    #[test]
    fn cli_parses_restore_subcommand_with_defaults_and_options() {
        let cli = Cli::try_parse_from(["pc-cleaner", "restore"]).unwrap();
        assert!(!cli.command.admin_requested());
        match cli.command {
            Command::Restore(args) => {
                assert_eq!(args.run, None);
                assert_eq!(args.path, None);
                assert!(!args.all_trash);
                assert!(!args.yes);
            }
            _ => panic!("expected Restore"),
        }

        let cli = Cli::try_parse_from([
            "pc-cleaner",
            "restore",
            "--run",
            "7",
            "--path",
            "a.txt",
            "--all-trash",
            "--yes",
        ])
        .unwrap();
        match cli.command {
            Command::Restore(args) => {
                assert_eq!(args.run, Some(7));
                assert_eq!(args.path, Some(PathBuf::from("a.txt")));
                assert!(args.all_trash);
                assert!(args.yes);
            }
            _ => panic!("expected Restore"),
        }
    }

    #[test]
    fn cli_parses_admin_flag_on_scan_and_clean() {
        let cli = Cli::try_parse_from(["pc-cleaner", "scan", "--admin"]).unwrap();
        assert!(cli.command.admin_requested());

        let cli = Cli::try_parse_from(["pc-cleaner", "clean", "--admin"]).unwrap();
        assert!(cli.command.admin_requested());

        let cli = Cli::try_parse_from(["pc-cleaner", "scan"]).unwrap();
        assert!(!cli.command.admin_requested());
    }

    #[test]
    fn elevated_marker_is_global_and_hidden_from_normal_use() {
        // サブコマンドの前でも後でも指定できること（内部用マーカーのため、
        // 利用者が意識する必要はないが、昇格後プロセスへの再付与が
        // 引数の並びに依存しないことを保証する）。
        let cli = Cli::try_parse_from(["pc-cleaner", "--elevated", "clean"]).unwrap();
        assert!(cli.elevated);

        let cli = Cli::try_parse_from(["pc-cleaner", "clean", "--elevated"]).unwrap();
        assert!(cli.elevated);

        let cli = Cli::try_parse_from(["pc-cleaner", "clean"]).unwrap();
        assert!(!cli.elevated);
    }

    #[test]
    fn relaunch_args_appends_elevated_marker() {
        let original = vec!["clean".to_string(), "--admin".to_string()];
        let relaunched = relaunch_args(&original);
        assert_eq!(
            relaunched,
            vec![
                "clean".to_string(),
                "--admin".to_string(),
                "--elevated".to_string()
            ]
        );

        // 引き継いだ引数列が再度 Cli としてパースできること（実際に
        // ShellExecuteExW へ渡す文字列を組み立てる前段の健全性チェック）。
        let mut argv = vec!["pc-cleaner".to_string()];
        argv.extend(relaunched);
        let cli = Cli::try_parse_from(argv).unwrap();
        assert!(cli.elevated);
        assert!(cli.command.admin_requested());
    }
}
