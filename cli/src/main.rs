//! pc-cleaner の薄い CLI（F-CLI-01〜08）。
//!
//! 判定・削除ロジックは一切持たず、`pc-cleaner-core` の呼び出しと表示のみを
//! 行う（F-CLI-01 / NF-MNT-01）。CLI で固めた挙動がそのまま GUI に乗る
//! （設計目標 G4 / F-CLI-08）。

#![deny(unsafe_code)]

use clap::{Args, Parser, Subcommand};
use pc_cleaner_core::{
    Config, DeleteMode, DeleteOutcome, DeletePlan, DeleteRequest, ItemOutcome, ScanEntry, config,
    execute, human_size, platform, preview, rules_for_safeties, safety_scope, scan_pipeline,
};
use std::io::{self, Write};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "pc-cleaner", about = "手動選択型ディスク掃除ツール", version)]
struct Cli {
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let platform = platform::current();
    let config = load_config(platform.as_ref());

    match cli.command {
        Command::Scan(args) => run_scan(platform.as_ref(), &config, args.all),
        Command::Clean(args) => run_clean(platform.as_ref(), &config, args),
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
            }
            _ => panic!("expected Clean"),
        }
    }
}
