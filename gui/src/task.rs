//! バックグラウンドスレッドでの走査・削除実行。
//!
//! `core` の `on_progress` コールバックは「呼び出し元スレッドから呼ばれる」
//! 契約だが、走査・削除自体を別スレッドで行うため、コールバックはその
//! ワーカースレッド上で走る。コールバックが GUI の状態（`App`）へ直接触れる
//! ことは型システム上できない（move できるのは `Sender` と `egui::Context`
//! だけ）。UI スレッドは `WorkerMsg` をチャネル経由で受け取り、
//! `update()` の冒頭でまとめて反映する（NF-PRF-02：走査中の進捗通知）。

use pc_cleaner_core::platform::{self, Platform, RestoreItemOutcome};
use pc_cleaner_core::{
    Config, DeleteOutcome, DeletePlan, DeleteProgress, RestoreOutcome, Rule, RuleSet, ScanEntry,
    ScanProgress, audit, delete, restore, scan_pipeline_with_progress,
};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::view::Scope;

/// ワーカースレッドから UI スレッドへ送られるメッセージ。
pub enum WorkerMsg {
    /// 走査の進捗イベント。
    Scan(ScanProgress),
    /// 走査完了。以後の `preview()` / 表示に使うルール一覧と走査結果。
    ScanDone {
        rules: Vec<Rule>,
        /// 現在の `scope`（Safe のみ／すべて）に関わらない、解決済みの全ルール
        /// （`RuleSet::all()`）。サイドバーのルール別既定・「管理者権限が
        /// 必要」一覧・ユーザー定義ルール件数の表示は、走査対象を絞り込む前の
        /// この一覧を使う（rules.json の上書き・追加ルールが scope で消えて
        /// 見えなくならないようにするため）。
        all_rules: Vec<Rule>,
        entries: Vec<ScanEntry>,
        /// ルール定義ファイル（D4 / Issue #54）の読み込み・検証で見つかった
        /// 問題。空なら警告なし。
        rule_issues: Vec<String>,
    },
    /// 削除の進捗イベント。
    Delete(DeleteProgress),
    /// 削除完了。
    DeleteDone(DeleteOutcome),
    /// 復元完了（D3 / Issue #53）。
    RestoreDone(RestoreOutcome),
}

/// 実行中の `Platform` を生成する。`demo` が `true` かつ `demo` feature が
/// 有効な場合のみサンドボックスの `DemoPlatform` を使う。
pub fn make_platform(demo: bool) -> Box<dyn Platform> {
    if demo {
        #[cfg(feature = "demo")]
        {
            return Box::new(crate::demo::DemoPlatform::new());
        }
        #[cfg(not(feature = "demo"))]
        {
            eprintln!("--demo is only effective when built with `--features demo`.");
        }
    }
    platform::current()
}

/// `scope` に応じて走査をバックグラウンドで実行する。
///
/// `scan_pipeline_with_progress`（走査 + `apply_rule_prefs`）を呼ぶだけで、
/// CLI の `scan_and_apply_prefs` と同じ core 呼び出し列を保つ（F-CLI-08）。
/// ルール一覧は `RuleSet::load`（組み込み + ルール定義ファイルの上書き・
/// 追加、D4 / Issue #54）で解決する。
pub fn spawn_scan(
    ctx: egui::Context,
    scope: Scope,
    config: Config,
    demo: bool,
) -> Receiver<WorkerMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let platform = make_platform(demo);
        let rule_set = RuleSet::load(platform.as_ref(), &config);
        let rule_issues = rule_set
            .issues()
            .iter()
            .map(|issue| issue.message.clone())
            .collect();
        let rules = rule_set.for_safeties(&scope.safeties(), platform.is_elevated());
        let all_rules = rule_set.all().to_vec();
        let progress_tx = tx.clone();
        let progress_ctx = ctx.clone();
        let entries =
            scan_pipeline_with_progress(platform.as_ref(), &rules, &config, move |event| {
                let _ = progress_tx.send(WorkerMsg::Scan(event));
                progress_ctx.request_repaint();
            });
        let _ = tx.send(WorkerMsg::ScanDone {
            rules,
            all_rules,
            entries,
            rule_issues,
        });
        ctx.request_repaint();
    });
    rx
}

/// `plan` の削除をバックグラウンドで実行する。`plan` はプレビュー段で
/// 生成済みの計画をそのまま move するため、実行前に再度プレビューを経ずに
/// 削除する経路は存在しない（F-DEL-01）。
pub fn spawn_delete(ctx: egui::Context, plan: DeletePlan, demo: bool) -> Receiver<WorkerMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let platform = make_platform(demo);
        let progress_tx = tx.clone();
        let progress_ctx = ctx.clone();
        let outcome = delete::execute_with_progress(platform.as_ref(), plan, move |event| {
            let _ = progress_tx.send(WorkerMsg::Delete(event));
            progress_ctx.request_repaint();
        });
        let _ = tx.send(WorkerMsg::DeleteDone(outcome));
        ctx.request_repaint();
    });
    rx
}

/// 指定した実行（`run_id`）分の削除をゴミ箱から復元する（D3 / Issue #53）。
///
/// 一覧のレビューは行わず、呼び出された時点で `run_id` に属する全項目を
/// 直ちに復元する。GUI では「この実行を元に戻す」ボタン1つで完結させる
/// ための設計であり、CLI の `restore`（既定は一覧のみ・`--yes` で確定）
/// より踏み込んだ操作になるため、呼び出し側（`app.rs`）で確認モーダルを
/// 経由してから呼ぶこと。
pub fn spawn_restore(ctx: egui::Context, run_id: u64, demo: bool) -> Receiver<WorkerMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let platform = make_platform(demo);
        let outcome = restore_run(platform.as_ref(), run_id);
        let _ = tx.send(WorkerMsg::RestoreDone(outcome));
        ctx.request_repaint();
    });
    rx
}

/// `spawn_restore` の本体。ゴミ箱一覧の取得に失敗した場合（未対応 OS 等）は、
/// その旨を1件の `Failed` 結果として返す（`RestoreOutcome` に「全体が失敗
/// した」を表すバリアントを別途設けるより、既存の項目単位の結果に載せる
/// ほうが `app.rs` 側の表示を1系統に保てるため）。
fn restore_run(platform: &dyn Platform, run_id: u64) -> RestoreOutcome {
    let trash = match platform.list_trash() {
        Ok(entries) => entries,
        Err(e) => {
            return RestoreOutcome {
                results: vec![(
                    PathBuf::new(),
                    RestoreItemOutcome::Failed {
                        message: e.to_string(),
                    },
                )],
            };
        }
    };

    let audit_records = audit::audit_file_path(platform)
        .and_then(|path| audit::load(&path).ok())
        .unwrap_or_default();

    let candidates = restore::correlate(&trash, &audit_records);
    let for_run: Vec<_> = restore::candidates_for_run(&candidates, run_id)
        .into_iter()
        .cloned()
        .collect();

    restore::restore(platform, &for_run)
}
