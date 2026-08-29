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

use clap::{Args, Parser, Subcommand};
use pc_cleaner_core::{
    Config, DeleteMode, DeleteOutcome, DeletePlan, DeleteRequest, ElevateError, ItemOutcome,
    ScanEntry, config, execute, human_size, platform, preview, rules_for_safeties, safety_scope,
    scan_pipeline, should_relaunch,
};
use std::io::{self, Write};
use std::process::ExitCode;

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
}

impl Command {
    /// `--admin` が指定されたか（Scan/Clean 共通）。
    fn admin_requested(&self) -> bool {
        match self {
            Command::Scan(args) => args.admin,
            Command::Clean(args) => args.admin,
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
        Command::Scan(args) => run_scan(platform.as_ref(), &config, args.all),
        Command::Clean(args) => run_clean(platform.as_ref(), &config, args),
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
fn scan_and_apply_prefs(
    platform: &dyn platform::Platform,
    config: &Config,
    all: bool,
) -> (Vec<pc_cleaner_core::Rule>, Vec<ScanEntry>) {
    let rules = rules_for_safeties(&safety_scope(all));
    let entries = scan_pipeline(platform, &rules, config);
    (rules, entries)
}

fn run_scan(platform: &dyn platform::Platform, config: &Config, all: bool) -> ExitCode {
    let (_rules, entries) = scan_and_apply_prefs(platform, config, all);
    print_scan_report(&entries);
    ExitCode::SUCCESS
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
    print_plan_summary(&plan);

    let final_plan = if args.permanent && !args.dry_run && !plan.is_empty() {
        if args.yes || confirm_permanent(plan.item_count(), plan.total_size()) {
            let confirmed_mode = DeleteMode::resolve(config, DeleteRequest::permanent_confirmed());
            preview(platform, &entries, &rules, confirmed_mode)
        } else {
            println!("キャンセルしました。");
            plan
        }
    } else {
        plan
    };

    let outcome = execute(platform, final_plan);
    print_outcome(&outcome);

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
            }
            _ => panic!("expected Clean"),
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
