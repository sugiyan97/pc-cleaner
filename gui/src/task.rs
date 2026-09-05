//! バックグラウンドスレッドでの走査・削除実行。
//!
//! `core` の `on_progress` コールバックは「呼び出し元スレッドから呼ばれる」
//! 契約だが、走査・削除自体を別スレッドで行うため、コールバックはその
//! ワーカースレッド上で走る。コールバックが GUI の状態（`App`）へ直接触れる
//! ことは型システム上できない（move できるのは `Sender` と `egui::Context`
//! だけ）。UI スレッドは `WorkerMsg` をチャネル経由で受け取り、
//! `update()` の冒頭でまとめて反映する（NF-PRF-02：走査中の進捗通知）。

use pc_cleaner_core::platform::{self, Platform};
use pc_cleaner_core::{
    Config, DeleteOutcome, DeletePlan, DeleteProgress, Rule, ScanEntry, ScanProgress, delete,
    rules_for_safeties, scan_pipeline_with_progress,
};
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
        entries: Vec<ScanEntry>,
    },
    /// 削除の進捗イベント。
    Delete(DeleteProgress),
    /// 削除完了。
    DeleteDone(DeleteOutcome),
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
            eprintln!("--demo は `--features demo` でビルドした場合のみ有効です。");
        }
    }
    platform::current()
}

/// `scope` に応じて走査をバックグラウンドで実行する。
///
/// `scan_pipeline_with_progress`（走査 + `apply_rule_prefs`）を呼ぶだけで、
/// CLI の `scan_and_apply_prefs` と同じ core 呼び出し列を保つ（F-CLI-08）。
pub fn spawn_scan(
    ctx: egui::Context,
    scope: Scope,
    config: Config,
    demo: bool,
) -> Receiver<WorkerMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let platform = make_platform(demo);
        let rules = rules_for_safeties(&scope.safeties(), &config, platform.is_elevated());
        let progress_tx = tx.clone();
        let progress_ctx = ctx.clone();
        let entries =
            scan_pipeline_with_progress(platform.as_ref(), &rules, &config, move |event| {
                let _ = progress_tx.send(WorkerMsg::Scan(event));
                progress_ctx.request_repaint();
            });
        let _ = tx.send(WorkerMsg::ScanDone { rules, entries });
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
