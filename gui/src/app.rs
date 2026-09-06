//! アプリケーション状態と `eframe::App` 実装。
//!
//! `core` の呼び出しと表示・操作のみを行い、判定・削除ロジックは持たない
//! （F-GUI-07）。走査・推奨の手順は CLI（`cli/src/main.rs` の
//! `scan_and_apply_prefs` / `run_clean`）と同一の core 呼び出し列に揃える
//! こと（F-CLI-08 / 9.5）。変更する場合は両方を直すこと。

use eframe::egui;
use pc_cleaner_core::platform::{Platform, RestoreItemOutcome};
use pc_cleaner_core::{
    BucketBreakdown, CategoryBreakdown, Config, DeleteMode, DeleteOutcome, DeletePlan,
    DeleteProgress, DeleteRequest, ElevateError, History, ItemOutcome, Lang, RestoreOutcome, Rule,
    ScanEntry, ScanProgress, SkipReason, audit, breakdown, config, history,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use crate::task::{self, WorkerMsg};
use crate::view::{self, Scope};

/// 上下左パネル（ツールバー・サイドバー・ステータスバー）の背景色。
/// 既定ではどのパネルも同じ `panel_fill` になり全体が単色に見えるため、
/// 中央パネル（白）より少し暗いグレーにして領域を分かりやすくする。
const CHROME_BG: egui::Color32 = egui::Color32::from_rgb(233, 236, 240);

enum Task {
    Idle,
    Scanning {
        rule_id: Option<String>,
        files_scanned: u64,
    },
    Deleting {
        done: usize,
        total: usize,
    },
    /// ゴミ箱からの復元中（D3 / Issue #53）。復元は通常一瞬で終わるため、
    /// 削除のような1件ごとの進捗通知は設けていない。
    Restoring,
}

struct OutcomeSummary {
    deleted: usize,
    failed: usize,
    freed_bytes: u64,
    dry_run: bool,
    failures: Vec<(PathBuf, String)>,
}

impl From<DeleteOutcome> for OutcomeSummary {
    fn from(outcome: DeleteOutcome) -> Self {
        let failures = outcome
            .results
            .iter()
            .filter_map(|r| match &r.outcome {
                ItemOutcome::Failed { message } => Some((r.path.clone(), message.clone())),
                _ => None,
            })
            .collect();
        OutcomeSummary {
            deleted: outcome.deleted_count(),
            failed: outcome.failed_count(),
            freed_bytes: outcome.freed_bytes(),
            dry_run: outcome.is_dry_run(),
            failures,
        }
    }
}

/// 復元結果の表示用サマリ（D3 / Issue #53）。`OutcomeSummary` と同じ考え方で、
/// `RestoreOutcome` の集計値をそのまま使う（GUI 側で合計を再計算しない）。
struct RestoreOutcomeSummary {
    restored: usize,
    skipped: usize,
    not_found: usize,
    failed: usize,
    failures: Vec<(PathBuf, String)>,
}

impl From<RestoreOutcome> for RestoreOutcomeSummary {
    fn from(outcome: RestoreOutcome) -> Self {
        let failures = outcome
            .results
            .iter()
            .filter_map(|(path, o)| match o {
                RestoreItemOutcome::Failed { message } => Some((path.clone(), message.clone())),
                _ => None,
            })
            .collect();
        RestoreOutcomeSummary {
            restored: outcome.restored_count(),
            skipped: outcome.skipped_count(),
            not_found: outcome.not_found_count(),
            failed: outcome.failed_count(),
            failures,
        }
    }
}

/// pc-cleaner の GUI 本体。
pub struct App {
    platform: Box<dyn Platform>,
    config_path: Option<PathBuf>,
    config: Config,
    history_path: Option<PathBuf>,
    history: History,
    /// 監査ログ（削除ログ、D1 / Issue #51）の保存先。`history_path` と同じく
    /// 起動時に一度だけ解決する。
    audit_path: Option<PathBuf>,
    /// 監査ログの内容。起動時に読み込み、削除実行のたびに追記分を足す
    /// （`history` と異なり毎回ファイル全体を読み直さない。`record_audit`
    /// 参照）。
    audit_records: Vec<pc_cleaner_core::AuditRecord>,
    /// ルール定義ファイル（`<config_dir>/rules.json` / D4 / Issue #54）の
    /// パス。`config_path` と同じく起動時に一度だけ解決する。実際の読込・
    /// 検証は走査ワーカー（`task::spawn_scan`）が毎回行う（`RuleSet` 自体は
    /// `App` に保持しない。ルール一覧は既存の `self.rules` で十分なため）。
    rules_path: Option<PathBuf>,
    demo: bool,
    /// 現在のプロセスが管理者権限で動作しているか（起動時に一度だけ判定し
    /// 保持する。実行中に変化しないため毎フレーム問い合わせる必要はない）。
    elevated: bool,
    /// 「管理者として実行し直す」確認モーダルを表示中か。
    confirming_elevation: bool,
    /// 「この実行を元に戻す」確認モーダルで対象にしている `run_id`
    /// （D3 / Issue #53）。`None` なら非表示。
    confirming_restore_run_id: Option<u64>,

    scope: Scope,
    rules: Vec<Rule>,
    /// 現在の `scope` に関わらない、解決済みの全ルール（`RuleSet::all()`）。
    /// サイドバーのルール別既定・「管理者権限が必要」一覧・ユーザー定義ルール
    /// 件数の表示に使う（`rules` は scope で絞り込まれているため、Safe のみ
    /// 走査中は Caution/Review やユーザー定義ルールの設定ができなくなって
    /// しまうのを避けるため。追随漏れ修正 / Issue #55）。
    all_rules: Vec<Rule>,
    entries: Vec<ScanEntry>,
    skipped: Vec<(String, SkipReason)>,

    plan: Option<DeletePlan>,
    plan_dirty: bool,
    exclusion_map: HashMap<PathBuf, &'static str>,
    /// `plan` の内訳（種類別 / サイズ別）。C3：プレビュー時の内訳表示。
    /// `plan` と同じタイミング（`recompute_plan`）で再計算するため、常に
    /// `plan` の中身と一致する（GUI 側では合計を足し算しない、F-GUI-07）。
    breakdown: Vec<CategoryBreakdown>,
    buckets: Vec<BucketBreakdown>,

    task: Task,
    rx: Option<Receiver<WorkerMsg>>,

    dry_run: bool,
    /// 完全削除（ゴミ箱を経由しない、復旧不可）を要求しているか。
    /// `true` の間は [`DeleteMode`] が常に [`DeleteMethod::Permanent`] になるが、
    /// 実際にドライランでなくなるのは確認モーダルで対象件数を入力し終えた後
    /// （`ui_confirm_modal` が `DeleteRequest::permanent_confirmed()` を使う）
    /// だけである（F-DEL-06）。
    permanent: bool,
    /// 完全削除確認モーダルで、対象件数の入力欄に入力中の文字列。
    permanent_confirm_text: String,
    confirming: bool,
    last_outcome: Option<OutcomeSummary>,
    /// 直近の復元結果（D3 / Issue #53）。
    last_restore_outcome: Option<RestoreOutcomeSummary>,
    notices: Vec<String>,
    started: bool,
}

impl App {
    /// `demo` が `true` の場合、`demo` feature が有効ならサンドボックスの
    /// `Platform` を使う（開発時の目視確認用）。`relaunched` は `--elevated`
    /// （内部用マーカー）付きで起動されたか（A2 / Issue #41）。
    pub fn new(demo: bool, relaunched: bool) -> Self {
        let platform = task::make_platform(demo);
        let elevated = platform.is_elevated();
        let config_path = config::config_file_path(platform.as_ref());
        let config = config_path.as_deref().map(config::load).unwrap_or_default();

        let history_path = history::history_file_path(platform.as_ref());
        let mut notices = Vec::new();
        // `history::load` は config::load と異なり壊れたファイルを黙って
        // 既定値へ差し替えない（実績は再現できないため）。ここで拾って
        // 通知するが、破損したファイルを空の履歴で上書き保存はしない
        // （`save()` を呼ばない）ので、ディスク上のファイルは調査用に残る。
        let lang = config.lang;
        let history = match history_path.as_deref().map(history::load) {
            Some(Ok(history)) => history,
            Some(Err(e)) => {
                notices.push(match lang {
                    Lang::Ja => format!(
                        "履歴の読み込みに失敗しました（{e}）。過去の実績が正しく表示されない場合があります。"
                    ),
                    Lang::En => format!(
                        "Failed to load history ({e}). Past results may not display correctly."
                    ),
                });
                History::default()
            }
            None => History::default(),
        };

        if relaunched && !elevated {
            // should_relaunch の二重防御が効いた場合。通常は起こらないが、
            // 起きた場合は静かに非昇格のまま続けるのではなく理由を伝える。
            notices.push(
                match lang {
                    Lang::Ja => {
                        "管理者権限で起動し直しましたが、昇格を確認できませんでした。\
                        管理者権限が必要な領域は対象外のままです。"
                    }
                    Lang::En => {
                        "Restarted with administrator privileges, but elevation could \
                        not be confirmed. Areas requiring administrator privileges remain \
                        excluded."
                    }
                }
                .to_string(),
            );
        }

        let audit_path = audit::audit_file_path(platform.as_ref());
        let audit_records = match audit_path.as_deref().map(audit::load) {
            Some(Ok(records)) => records,
            Some(Err(e)) => {
                notices.push(match lang {
                    Lang::Ja => format!("削除ログの読み込みに失敗しました（{e}）。"),
                    Lang::En => format!("Failed to load the deletion log ({e})."),
                });
                Vec::new()
            }
            None => Vec::new(),
        };

        let rules_path = pc_cleaner_core::ruleset::rules_file_path(platform.as_ref());

        App {
            platform,
            config_path,
            config,
            history_path,
            history,
            audit_path,
            audit_records,
            rules_path,
            demo,
            elevated,
            confirming_elevation: false,
            confirming_restore_run_id: None,
            scope: Scope::SafeOnly,
            rules: Vec::new(),
            all_rules: Vec::new(),
            entries: Vec::new(),
            skipped: Vec::new(),
            plan: None,
            plan_dirty: true,
            exclusion_map: HashMap::new(),
            breakdown: Vec::new(),
            buckets: Vec::new(),
            task: Task::Idle,
            rx: None,
            dry_run: false,
            permanent: false,
            permanent_confirm_text: String::new(),
            confirming: false,
            last_outcome: None,
            last_restore_outcome: None,
            notices,
            started: false,
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        self.task = Task::Scanning {
            rule_id: None,
            files_scanned: 0,
        };
        self.plan = None;
        self.plan_dirty = true;
        self.exclusion_map.clear();
        self.rx = Some(task::spawn_scan(
            ctx.clone(),
            self.scope,
            self.config.clone(),
            self.demo,
        ));
    }

    fn poll_worker(&mut self) {
        let Some(rx) = self.rx.take() else { return };
        let mut keep_receiver = true;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMsg::Scan(ScanProgress::RuleStarted { rule_id, .. }) => {
                    if let Task::Scanning { rule_id: r, .. } = &mut self.task {
                        *r = Some(rule_id);
                    }
                }
                WorkerMsg::Scan(ScanProgress::Walking { files_scanned, .. }) => {
                    if let Task::Scanning {
                        files_scanned: f, ..
                    } = &mut self.task
                    {
                        *f = files_scanned;
                    }
                }
                WorkerMsg::Scan(ScanProgress::RuleSkipped { rule_id, reason }) => {
                    self.skipped.push((rule_id, reason));
                }
                WorkerMsg::Scan(_) => {}
                WorkerMsg::ScanDone {
                    rules,
                    all_rules,
                    entries,
                    rule_issues,
                } => {
                    self.rules = rules;
                    self.all_rules = all_rules;
                    self.entries = entries;
                    // ルール定義ファイル（D4 / Issue #54）の検証で見つかった
                    // 問題は、履歴・監査ログの読込失敗と同じく notices へ積む
                    // だけで、走査自体は組み込みルールのみで続行済み。再走査の
                    // たびに同じ問題を積み増さないよう、前回分は入れ替える
                    // （追随漏れ修正 / Issue #55）。
                    self.notices.retain(|n| !n.starts_with("ルール定義: "));
                    for issue in rule_issues {
                        self.notices.push(format!("ルール定義: {issue}"));
                    }
                    self.task = Task::Idle;
                    self.plan_dirty = true;
                    keep_receiver = false;
                }
                WorkerMsg::Delete(DeleteProgress::ItemFinished { done, total, .. }) => {
                    self.task = Task::Deleting { done, total };
                }
                WorkerMsg::Delete(_) => {}
                WorkerMsg::DeleteDone(outcome) => {
                    if !outcome.is_dry_run() {
                        let run_id = audit::next_run_id(SystemTime::now());
                        self.record_history(&outcome, run_id);
                        self.record_audit(&outcome, run_id);
                    }
                    self.last_outcome = Some(outcome.into());
                    self.task = Task::Idle;
                    // 削除済みエントリが一覧に残らないよう自動で再走査する。
                    self.entries.clear();
                    self.skipped.clear();
                    self.plan = None;
                    self.plan_dirty = true;
                    keep_receiver = false;
                }
                WorkerMsg::RestoreDone(outcome) => {
                    self.last_restore_outcome = Some(outcome.into());
                    self.task = Task::Idle;
                    keep_receiver = false;
                }
            }
        }
        if keep_receiver {
            self.rx = Some(rx);
        }
    }

    fn is_busy(&self) -> bool {
        !matches!(self.task, Task::Idle)
    }

    /// 選択の再集計（F-GUI-05）。core の `preview()` を通した結果のみを
    /// 真実の値として使う（GUI 側で合計を足し算しない）。
    ///
    /// `permanent` が立っている間は必ず `permanent_unconfirmed()` を使う
    /// （CLI の `run_clean` が `--permanent` 単体でまず未確定プレビューを
    /// 見せるのと同じ設計）。実際に確認済みの完全削除計画を作るのは
    /// `ui_confirm_modal` の役目であり、ここでは絶対に確定させない。
    fn recompute_plan(&mut self) {
        let request = match (self.dry_run, self.permanent) {
            (_, true) => DeleteRequest::permanent_unconfirmed(),
            (true, false) => DeleteRequest::dry_run(),
            (false, false) => DeleteRequest::execute(),
        };
        let mode = DeleteMode::resolve(&self.config, request);
        let plan =
            pc_cleaner_core::preview(self.platform.as_ref(), &self.entries, &self.rules, mode);
        self.exclusion_map = view::to_exclusion_map(
            plan.excluded().iter().map(|e| (e.path.clone(), e.reason)),
            self.config.lang,
        );
        let breakdown_items = breakdown::from_plan(&plan);
        self.breakdown = breakdown::by_rule(&breakdown_items);
        self.buckets = breakdown::by_size_bucket(&breakdown_items);
        self.plan = Some(plan);
        self.plan_dirty = false;
    }

    fn save_config(&mut self) {
        let lang = self.config.lang;
        if let Some(path) = &self.config_path {
            if let Err(e) = config::save(&self.config, path) {
                self.notices.push(match lang {
                    Lang::Ja => format!("設定の保存に失敗しました: {e}"),
                    Lang::En => format!("Failed to save settings: {e}"),
                });
            }
        } else {
            self.notices.push(
                match lang {
                    Lang::Ja => "この環境では設定の保存先を特定できません。",
                    Lang::En => "Could not determine where to save settings on this system.",
                }
                .to_string(),
            );
        }
    }

    /// 削除結果を履歴（`history.json`）へ記録する（C4 / Issue #50）。
    /// 記録に失敗しても `notices` へ警告を積むだけで、アプリの他の動作は
    /// 妨げない（`save_config` と同じ方針）。
    fn record_history(&mut self, outcome: &DeleteOutcome, run_id: u64) {
        let lang = self.config.lang;
        match history::record_outcome(self.platform.as_ref(), outcome, run_id) {
            None => {
                self.notices.push(
                    match lang {
                        Lang::Ja => "この環境では履歴の保存先を特定できません。",
                        Lang::En => "Could not determine where to save history on this system.",
                    }
                    .to_string(),
                );
            }
            Some(Err(e)) => {
                self.notices.push(match lang {
                    Lang::Ja => format!("履歴の記録に失敗しました: {e}"),
                    Lang::En => format!("Failed to record history: {e}"),
                });
            }
            Some(Ok(history)) => {
                self.history = history;
            }
        }
    }

    /// 削除結果を監査ログ（`deletion_log.jsonl`）へ記録する（D1 / Issue #51）。
    /// `record_history` と同じ方針：記録に失敗しても `notices` へ警告を積む
    /// だけで、アプリの他の動作は妨げない。
    fn record_audit(&mut self, outcome: &DeleteOutcome, run_id: u64) {
        let lang = self.config.lang;
        match audit::record_outcome(self.platform.as_ref(), &self.config, outcome, run_id) {
            None => {
                self.notices.push(
                    match lang {
                        Lang::Ja => "この環境では削除ログの保存先を特定できません。",
                        Lang::En => {
                            "Could not determine where to save the deletion log on this system."
                        }
                    }
                    .to_string(),
                );
            }
            Some(Err(e)) => {
                self.notices.push(match lang {
                    Lang::Ja => format!("削除ログの記録に失敗しました: {e}"),
                    Lang::En => format!("Failed to record the deletion log: {e}"),
                });
            }
            Some(Ok(_)) => {
                if self.config.audit_log_enabled {
                    self.audit_records.extend(audit::records_from_outcome(
                        outcome,
                        run_id,
                        SystemTime::now(),
                    ));
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.started {
            self.started = true;
            self.start_scan(ctx); // F-GUI-04: 起動直後に Safe ルールを走査する
        }

        self.poll_worker();
        if self.plan_dirty && !self.is_busy() {
            self.recompute_plan();
        }

        self.ui_top(ctx);
        self.ui_side(ctx);
        self.ui_bottom(ctx);
        self.ui_central(ctx);
        self.ui_confirm_modal(ctx);
        self.ui_elevate_modal(ctx);
        self.ui_restore_confirm_modal(ctx);
    }
}

impl App {
    fn ui_top(&mut self, ctx: &egui::Context) {
        // 既定では上下左パネルも中央と同じ panel_fill になり、全体が単色に
        // 見えてしまう（ユーザー指摘）。中央パネルより少し暗いグレーにして
        // 領域の区切りを分かりやすくする。
        let frame = egui::Frame::side_top_panel(&ctx.style()).fill(CHROME_BG);
        egui::TopBottomPanel::top("top")
            .frame(frame)
            .show(ctx, |ui| {
                let lang = self.config.lang;
                ui.horizontal(|ui| {
                    ui.heading("pc-cleaner");
                    ui.separator();
                    let busy = self.is_busy();
                    ui.add_enabled_ui(!busy, |ui| {
                        let mut changed = false;
                        for scope in [Scope::SafeOnly, Scope::All] {
                            if ui
                                .radio_value(&mut self.scope, scope, scope.label(lang))
                                .changed()
                            {
                                changed = true;
                            }
                        }
                        let rescan_label = match lang {
                            Lang::Ja => "再走査",
                            Lang::En => "Rescan",
                        };
                        if ui.button(rescan_label).clicked() {
                            changed = true;
                        }
                        if changed {
                            self.start_scan(ctx);
                        }
                    });
                    ui.separator();
                    if self.elevated {
                        ui.label(match lang {
                            Lang::Ja => "🔓 管理者権限で実行中",
                            Lang::En => "🔓 Running with administrator privileges",
                        });
                    } else {
                        ui.add_enabled_ui(!self.is_busy(), |ui| {
                            let restart_label = match lang {
                                Lang::Ja => "🔒 管理者として実行し直す",
                                Lang::En => "🔒 Restart as administrator",
                            };
                            if ui
                                .button(restart_label)
                                .on_hover_text(view::elevate_confirm_text(lang))
                                .clicked()
                            {
                                self.confirming_elevation = true;
                            }
                        });
                    }
                    match &self.task {
                        Task::Scanning {
                            rule_id,
                            files_scanned,
                        } => {
                            ui.spinner();
                            ui.label(match lang {
                                Lang::Ja => format!(
                                    "走査中: {} … {files_scanned} 件",
                                    rule_id.as_deref().unwrap_or("準備中")
                                ),
                                Lang::En => format!(
                                    "Scanning: {} … {files_scanned} items",
                                    rule_id.as_deref().unwrap_or("preparing")
                                ),
                            });
                        }
                        Task::Deleting { done, total } => {
                            ui.spinner();
                            ui.label(match lang {
                                Lang::Ja => format!("削除中: {done} / {total} 件"),
                                Lang::En => format!("Deleting: {done} / {total} items"),
                            });
                        }
                        Task::Restoring => {
                            ui.spinner();
                            ui.label(match lang {
                                Lang::Ja => "復元中…",
                                Lang::En => "Restoring…",
                            });
                        }
                        Task::Idle => {}
                    }
                });
                for notice in &self.notices {
                    ui.colored_label(ui.visuals().warn_fg_color, notice);
                }
            });
    }

    fn ui_side(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::side_top_panel(&ctx.style()).fill(CHROME_BG);
        egui::SidePanel::left("rules").frame(frame).show(ctx, |ui| {
            let lang = self.config.lang;

            egui::ScrollArea::vertical().show(ui, |ui| {
                // 表示言語の切り替え（E4 / Issue #58）。ルール一覧の label/description
                // は builtin_rules(config) が生成するため、切り替え後は再走査して
                // 反映する。
                let mut lang_changed = false;
                egui::ComboBox::from_id_salt("lang_select")
                    .selected_text(match self.config.lang {
                        Lang::Ja => "日本語",
                        Lang::En => "English",
                    })
                    .show_ui(ui, |ui| {
                        for candidate in [Lang::Ja, Lang::En] {
                            if ui
                                .selectable_value(
                                    &mut self.config.lang,
                                    candidate,
                                    match candidate {
                                        Lang::Ja => "日本語",
                                        Lang::En => "English",
                                    },
                                )
                                .changed()
                            {
                                lang_changed = true;
                            }
                        }
                    });
                if lang_changed {
                    self.save_config();
                    self.start_scan(ctx);
                }
                ui.separator();

                ui.heading(match lang {
                    Lang::Ja => "ルール別の既定",
                    Lang::En => "Rule defaults",
                });
                ui.label(match lang {
                    Lang::Ja => "常に選択 / 除外 / 毎回確認(推奨に従う)を設定できます。",
                    Lang::En => {
                        "You can set Always select / Always exclude / Ask each time \
                    (follows the recommendation) per rule."
                    }
                });
                ui.separator();

                let mut changed_rule: Option<String> = None;
                let mut changed_threshold_rule_id: Option<String> = None;
                // `.show()` に渡すクロージャ内で `&mut self.config` を書き換えつつ
                // `self.config` を読んで作った `Vec<Rule>` を同時に借用すると
                // 競合するため、先にルール一覧をローカル変数へ取り出しておく
                // （app.rs 内の他の `.show()` 呼び出しと同じパターン）。
                //
                // `self.rules`（現在の scope に絞り込み済み）ではなく
                // `self.all_rules`（`RuleSet::all()`。rules.json の上書き・追加
                // ルールを含む）を基点にする。Safe のみ走査中でも Caution /
                // Review ルールの既定やユーザー定義ルールを設定できるようにし、
                // かつラベルが rules.json の上書きに追随するようにするため
                // （追随漏れ修正 / Issue #55）。
                let scannable_rules: Vec<Rule> = self
                    .all_rules
                    .iter()
                    .filter(|r| r.is_permitted(self.elevated))
                    .cloned()
                    .collect();
                for rule in &scannable_rules {
                    ui.label(&rule.label).on_hover_text(&rule.description);
                    let mut pref = self
                        .config
                        .rule_prefs
                        .get(&rule.id)
                        .copied()
                        .unwrap_or_default();
                    egui::ComboBox::from_id_salt(&rule.id)
                        .selected_text(pref_label(pref, lang))
                        .show_ui(ui, |ui| {
                            for candidate in [
                                pc_cleaner_core::RulePref::AskEachTime,
                                pc_cleaner_core::RulePref::AlwaysSelect,
                                pc_cleaner_core::RulePref::Exclude,
                            ] {
                                if ui
                                    .selectable_value(
                                        &mut pref,
                                        candidate,
                                        pref_label(candidate, lang),
                                    )
                                    .changed()
                                {
                                    self.config.rule_prefs.insert(rule.id.clone(), pref);
                                    changed_rule = Some(rule.id.clone());
                                }
                            }
                        });
                    if let Some(default_days) = rule.age_threshold_days {
                        let mut days = self.config.age_threshold_days(&rule.id, default_days);
                        ui.horizontal(|ui| {
                            ui.label(match lang {
                                Lang::Ja => "しきい値（日）:",
                                Lang::En => "Threshold (days):",
                            });
                            let resp = ui.add(egui::DragValue::new(&mut days).range(
                                pc_cleaner_core::config::AGE_THRESHOLD_MIN_DAYS
                                    ..=pc_cleaner_core::config::AGE_THRESHOLD_MAX_DAYS,
                            ));
                            if resp.drag_stopped() || resp.lost_focus() {
                                self.config.age_thresholds.insert(rule.id.clone(), days);
                                changed_threshold_rule_id = Some(rule.id.clone());
                            }
                        });
                        ui.small(match lang {
                            Lang::Ja => "変更すると再走査します。",
                            Lang::En => "Changing this will trigger a rescan.",
                        });
                    }
                    ui.add_space(4.0);
                }
                if let Some(rule_id) = changed_rule {
                    view::reapply_pref_for_rule(&mut self.entries, &rule_id, &self.config);
                    self.plan_dirty = true;
                    self.save_config();
                }
                if changed_threshold_rule_id.is_some() {
                    // old_downloads は走査時（scan.rs）にしきい値でフィルタするため、
                    // reapply_pref_for_rule（走査済みエントリへの選択反映のみ）では
                    // 不十分。しきい値の変更は必ず再走査で反映する。
                    self.save_config();
                    self.start_scan(ctx);
                }

                ui.separator();
                // 他の設定項目（rule_prefs・age_thresholds・大容量しきい値等）と
                // 挙動を揃え、この3項目も変更時に自動保存する（「設定を保存」
                // ボタンを押し忘れると次回起動時に消える不整合の解消 / 追随漏れ
                // 修正 / Issue #55）。
                let mut settings_changed = false;
                if ui
                    .checkbox(
                        &mut self.config.use_trash,
                        match lang {
                            Lang::Ja => "ゴミ箱経由で削除する",
                            Lang::En => "Delete via Recycle Bin",
                        },
                    )
                    .on_hover_text(match lang {
                        Lang::Ja => {
                            "オフにすると通常の削除は実行できず、プレビューのみになります。"
                        }
                        Lang::En => {
                            "When off, regular deletion cannot be executed; only preview is \
                        available."
                        }
                    })
                    .changed()
                {
                    self.plan_dirty = true;
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut self.config.dry_run_default,
                        match lang {
                            Lang::Ja => "既定でドライラン",
                            Lang::En => "Dry run by default",
                        },
                    )
                    .changed()
                {
                    self.plan_dirty = true;
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut self.config.audit_log_enabled,
                        match lang {
                            Lang::Ja => "削除ログを記録する",
                            Lang::En => "Record a deletion log",
                        },
                    )
                    .on_hover_text(match lang {
                        Lang::Ja => {
                            "いつ何を削除したかをパス付きで記録します（誤削除の追跡用）。\
                         「これまでの実績」とは別ファイルで、無効化すると新規記録は行われません。"
                        }
                        Lang::En => {
                            "Records what was deleted and when, with paths (for tracking \
                        accidental deletions). This is a separate file from \"Past \
                        results\"; disabling it stops new records from being added."
                        }
                    })
                    .changed()
                {
                    settings_changed = true;
                }
                if settings_changed {
                    self.save_config();
                }

                ui.separator();
                // 大容量ファイルのしきい値（C2 / Issue #48）。Config はバイト単位で
                // 保持するが、入力は MB 単位のほうが扱いやすいためここで変換する。
                // 変更は次回の走査から反映される（rule.large_file_threshold_bytes
                // は走査時に builtin_rules() が Config から埋め込むため）。
                let mut large_file_mb =
                    (self.config.large_file_threshold_bytes / (1024 * 1024)).max(1);
                let mut rescan_needed = false;
                ui.horizontal(|ui| {
                    ui.label(match lang {
                        Lang::Ja => "大容量ファイルのしきい値（MB）:",
                        Lang::En => "Large file threshold (MB):",
                    });
                    if ui
                        .add(egui::DragValue::new(&mut large_file_mb).range(1..=1_048_576))
                        .on_hover_text(match lang {
                            Lang::Ja => {
                                "この値以上のファイルには「サイズが大きい」注意書きが付きます。"
                            }
                            Lang::En => "Files at or above this size get a \"large file\" note.",
                        })
                        .changed()
                    {
                        self.config.large_file_threshold_bytes = large_file_mb * 1024 * 1024;
                        rescan_needed = true;
                    }
                });
                if ui
                    .checkbox(
                        &mut self.config.detect_duplicates,
                        match lang {
                            Lang::Ja => "重複ファイルを検出する（走査が遅くなります）",
                            Lang::En => "Detect duplicate files (slows down scanning)",
                        },
                    )
                    .changed()
                {
                    rescan_needed = true;
                }
                if ui
                .checkbox(
                    &mut self.config.user_rules_enabled,
                    match lang {
                        Lang::Ja => "ルール定義ファイルを読み込む",
                        Lang::En => "Load rule definition file",
                    },
                )
                .on_hover_text(match lang {
                    Lang::Ja => {
                        "rules.json による組み込みルールの上書き・追加ルールを反映します（D4）。\
                         オフにすると組み込みルールのみで走査します。"
                    }
                    Lang::En => {
                        "Applies overrides and additional rules from rules.json (D4). When \
                        off, scans use only the built-in rules."
                    }
                })
                .changed()
            {
                rescan_needed = true;
            }
                if rescan_needed {
                    self.save_config();
                    self.start_scan(ctx);
                }

                if ui
                    .button(match lang {
                        Lang::Ja => "設定を保存",
                        Lang::En => "Save settings",
                    })
                    .clicked()
                {
                    self.save_config();
                }
                if let Some(path) = &self.config_path {
                    ui.small(match lang {
                        Lang::Ja => format!("保存先: {}", path.display()),
                        Lang::En => format!("Saved to: {}", path.display()),
                    });
                }

                ui.separator();
                egui::CollapsingHeader::new(match lang {
                    Lang::Ja => "これまでの実績",
                    Lang::En => "Past results",
                })
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(match lang {
                        Lang::Ja => format!(
                            "累計 {} を解放（{} 回）",
                            view::human_size(self.history.total_freed_bytes()),
                            self.history.run_count()
                        ),
                        Lang::En => format!(
                            "{} freed in total ({} runs)",
                            view::human_size(self.history.total_freed_bytes()),
                            self.history.run_count()
                        ),
                    });
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let restore_label = match lang {
                        Lang::Ja => "🔄 元に戻す",
                        Lang::En => "🔄 Undo",
                    };
                    for row in view::history_rows(&self.history, now_secs, 5, lang) {
                        ui.horizontal(|ui| {
                            ui.label(format!("{}: {}", row.when, row.summary));
                            // run_id == 0 は #51 より前の実績で監査ログとの
                            // 対応が無く、復元候補を特定できない
                            // （HistoryRow::run_id のドキュメント参照）。
                            if row.run_id != 0 && ui.small_button(restore_label).clicked() {
                                self.confirming_restore_run_id = Some(row.run_id);
                            }
                        });
                    }
                    if let Some(path) = &self.history_path {
                        ui.small(match lang {
                            Lang::Ja => format!("保存先: {}", path.display()),
                            Lang::En => format!("Saved to: {}", path.display()),
                        });
                    }
                    if let Some(summary) = &self.last_restore_outcome {
                        ui.separator();
                        ui.label(match lang {
                        Lang::Ja => format!(
                            "直近の復元: 成功 {} 件 / スキップ {} 件 / 対象消失 {} 件 / 失敗 {} 件",
                            summary.restored, summary.skipped, summary.not_found, summary.failed
                        ),
                        Lang::En => format!(
                            "Last restore: {} succeeded / {} skipped / {} not found / {} failed",
                            summary.restored, summary.skipped, summary.not_found, summary.failed
                        ),
                    });
                        for (path, message) in &summary.failures {
                            ui.colored_label(
                                ui.visuals().error_fg_color,
                                format!("  {}: {message}", path.display()),
                            );
                        }
                    }
                });

                ui.separator();
                egui::CollapsingHeader::new(match lang {
                    Lang::Ja => "削除ログ",
                    Lang::En => "Deletion log",
                })
                .default_open(false)
                .show(ui, |ui| {
                    if !self.config.audit_log_enabled {
                        ui.small(match lang {
                        Lang::Ja => {
                            "記録を無効化しています（上の「削除ログを記録する」で再開できます）。"
                        }
                        Lang::En => {
                            "Recording is disabled (re-enable it above with \"Record a \
                            deletion log\")."
                        }
                    });
                    }
                    ui.label(match lang {
                        Lang::Ja => format!("{} 件を記録", self.audit_records.len()),
                        Lang::En => format!("{} records", self.audit_records.len()),
                    });
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    for row in view::audit_rows(&self.audit_records, now_secs, 5, lang) {
                        ui.label(format!("{}: {}", row.when, row.summary))
                            .on_hover_text(&row.path);
                    }
                    if let Some(path) = &self.audit_path {
                        ui.small(match lang {
                            Lang::Ja => format!("保存先: {}", path.display()),
                            Lang::En => format!("Saved to: {}", path.display()),
                        });
                    }
                });

                ui.separator();
                egui::CollapsingHeader::new(match lang {
                    Lang::Ja => "ルール定義",
                    Lang::En => "Rule definitions",
                })
                .default_open(false)
                .show(ui, |ui| {
                    let user_defined = self.all_rules.iter().filter(|r| r.is_user_defined).count();
                    ui.label(match lang {
                        Lang::Ja => format!("ユーザー定義ルール: {user_defined} 件"),
                        Lang::En => format!("User-defined rules: {user_defined}"),
                    });
                    if let Some(path) = &self.rules_path {
                        ui.small(match lang {
                            Lang::Ja => format!("読込元: {}", path.display()),
                            Lang::En => format!("Loaded from: {}", path.display()),
                        });
                    }
                });

                let future_rules = view::future_rules(&self.all_rules, self.elevated);
                if !future_rules.is_empty() {
                    ui.separator();
                    ui.heading(match lang {
                        Lang::Ja => "管理者権限が必要（未昇格のため対象外）",
                        Lang::En => "Requires administrator privileges (excluded until elevated)",
                    });
                    for rule in &future_rules {
                        ui.label(format!("🔒 {}", rule.label))
                            .on_hover_text(&rule.description);
                    }
                }

                if !self.skipped.is_empty() {
                    ui.separator();
                    ui.heading(match lang {
                        Lang::Ja => "スキップされたルール",
                        Lang::En => "Skipped rules",
                    });
                    for (rule_id, reason) in &self.skipped {
                        ui.label(format!(
                            "{rule_id}: {}",
                            view::skip_reason_label(reason, lang)
                        ));
                    }
                }
            });
        });
    }

    fn ui_central(&mut self, ctx: &egui::Context) {
        // `.show()` に渡すクロージャ内で `self` のフィールドを個別に借用すると
        // （`index` が `self.rules` を、本文が `&mut self.entries` を要求する等）
        // 借用が競合するため、必要な値をあらかじめローカル変数へ取り出してから
        // クロージャに渡す。
        let rule_index: HashMap<String, Rule> = self
            .rules
            .iter()
            .map(|r| (r.id.clone(), r.clone()))
            .collect();
        let busy = self.is_busy();
        let exclusion_map = self.exclusion_map.clone();
        let mut entries = std::mem::take(&mut self.entries);
        let mut plan_dirty = false;
        let lang = self.config.lang;

        let frame = egui::Frame::central_panel(&ctx.style()).fill(egui::Color32::WHITE);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if entries.is_empty() && !busy {
                    ui.label(match lang {
                        Lang::Ja => "対象なし。",
                        Lang::En => "No items.",
                    });
                }
                for entry in &mut entries {
                    let rule = rule_index.get(&entry.rule_id);
                    let safety = rule.map(|r| r.safety);
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut entry.selected, "").changed() {
                            plan_dirty = true;
                        }
                        if let Some(safety) = safety {
                            // 「安全」も注意/要確認と同じく意味を持つ色で示す。
                            // 既定の weak_text_color は意図的に低コントラストで
                            // 読みづらいため、明示的な緑を使う。
                            const SAFE_COLOR: egui::Color32 = egui::Color32::from_rgb(21, 115, 71);
                            let color = match safety {
                                pc_cleaner_core::Safety::Safe => SAFE_COLOR,
                                pc_cleaner_core::Safety::Caution => ui.visuals().warn_fg_color,
                                pc_cleaner_core::Safety::Review => ui.visuals().error_fg_color,
                            };
                            ui.colored_label(color, view::safety_label(safety, lang));
                        }
                        ui.label(rule.map(|r| r.label.as_str()).unwrap_or(&entry.rule_id));
                        ui.label(view::human_size(entry.size));
                        ui.label(view::age_label(entry.age_days, lang));
                        // 大容量ファイル・重複ファイルの注意書き（C2 / Issue #48）。
                        // 判定ロジックは持たず view の純粋ヘルパーに委譲する
                        // （F-GUI-07）。
                        let large_file_threshold = rule.and_then(|r| r.large_file_threshold_bytes);
                        if let Some(badge) =
                            view::large_file_badge(entry.size, large_file_threshold, lang)
                        {
                            ui.colored_label(ui.visuals().warn_fg_color, badge);
                        }
                        if let Some(badge) = view::duplicate_badge(entry.duplicate, lang) {
                            ui.colored_label(ui.visuals().warn_fg_color, badge);
                        }
                        if let Some(hint) = exclusion_map.get(&entry.path) {
                            ui.colored_label(ui.visuals().warn_fg_color, *hint);
                        }
                    });
                    // パスは行内に置くと横幅を超えて見切れるため、独立した行に
                    // 出して CentralPanel の幅に収める。
                    ui.small(entry.path.display().to_string());
                    if !entry.reason.is_empty() {
                        ui.small(match lang {
                            Lang::Ja => format!("理由: {}", entry.reason),
                            Lang::En => format!("Reason: {}", entry.reason),
                        });
                    }
                    ui.separator();
                }
            });
        });

        self.entries = entries;
        if plan_dirty {
            self.plan_dirty = true;
        }
    }

    fn ui_bottom(&mut self, ctx: &egui::Context) {
        // ui_central と同様、`.show()` クロージャ内で self の複数フィールドを
        // 同時に借用しないよう、必要な値を先に取り出す。
        let plan_summary = self.plan.as_ref().map(|p| {
            let needs_permanent = p
                .excluded()
                .iter()
                .any(|e| e.reason == pc_cleaner_core::ExclusionReason::RequiresPermanent);
            (
                p.item_count(),
                p.total_size(),
                p.is_empty(),
                p.excluded().len(),
                needs_permanent,
            )
        });
        let busy = self.is_busy();
        let mut confirm_clicked = false;
        let mut plan_dirty = false;
        let mut clear_outcome_clicked = false;
        let lang = self.config.lang;

        // 内訳（C3）。`self.breakdown` / `self.buckets` は `recompute_plan` で
        // `self.plan` と同時に更新されるため、常に現在の選択状態を反映する。
        let rule_index: HashMap<String, Rule> = self
            .rules
            .iter()
            .map(|r| (r.id.clone(), r.clone()))
            .collect();
        let total_size_for_breakdown = plan_summary.map(|s| s.1).unwrap_or(0);
        let category_breakdown_rows =
            view::category_rows(&self.breakdown, &rule_index, total_size_for_breakdown, lang);
        let bucket_breakdown_rows =
            view::bucket_rows(&self.buckets, total_size_for_breakdown, lang);

        let frame = egui::Frame::side_top_panel(&ctx.style()).fill(CHROME_BG);
        egui::TopBottomPanel::bottom("bottom")
            .frame(frame)
            .show(ctx, |ui| {
            if let Some((item_count, total_size, is_empty, excluded_len, needs_permanent)) =
                plan_summary
            {
                ui.horizontal(|ui| {
                    ui.label(match lang {
                        Lang::Ja => format!(
                            "選択中: {item_count} 件（{}）を解放",
                            view::human_size(total_size)
                        ),
                        Lang::En => format!(
                            "Selected: {item_count} items ({}) to free",
                            view::human_size(total_size)
                        ),
                    });
                    if ui
                        .checkbox(
                            &mut self.dry_run,
                            match lang {
                                Lang::Ja => "削除せず確認だけ（ドライラン）",
                                Lang::En => "Preview only, don't delete (dry run)",
                            },
                        )
                        .changed()
                    {
                        plan_dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.permanent,
                            match lang {
                                Lang::Ja => "完全削除（ゴミ箱を経由しない、復旧不可）",
                                Lang::En => {
                                    "Permanently delete (bypass Recycle Bin, cannot be undone)"
                                }
                            },
                        )
                        .changed()
                    {
                        plan_dirty = true;
                    }
                    let confirm_label = match lang {
                        Lang::Ja => "確定",
                        Lang::En => "Confirm",
                    };
                    if ui
                        .add_enabled(!busy && !is_empty, egui::Button::new(confirm_label))
                        .clicked()
                    {
                        confirm_clicked = true;
                    }
                });
                if self.permanent {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        match lang {
                            Lang::Ja => {
                                "完全削除は復元できません。確定時にあらためて確認します。"
                            }
                            Lang::En => {
                                "Permanent deletion cannot be undone. You will be asked to \
                                confirm again when you proceed."
                            }
                        },
                    );
                }
                if excluded_len > 0 {
                    ui.small(match lang {
                        Lang::Ja => format!(
                            "除外: {excluded_len} 件（選択されていない・許可リスト外・要管理者権限など）"
                        ),
                        Lang::En => format!(
                            "Excluded: {excluded_len} items (not selected, outside the \
                            allowlist, requires administrator privileges, etc.)"
                        ),
                    });
                    if needs_permanent && !self.permanent {
                        ui.small(view::recycle_bin_exclusion_hint(lang));
                    }
                }
                if !is_empty {
                    // F-GUI-08: 確認モーダルと同じく種類別・サイズ帯別の両方を
                    // 画面下部でも表示する（追随漏れ修正 / Issue #55）。
                    egui::CollapsingHeader::new(match lang {
                        Lang::Ja => "内訳（種類別）",
                        Lang::En => "Breakdown (by type)",
                    })
                    .id_salt("bottom_breakdown_category")
                    .default_open(false)
                    .show(ui, |ui| {
                        for row in &category_breakdown_rows {
                            ui.add(
                                egui::ProgressBar::new(row.fraction)
                                    .desired_width(260.0)
                                    .text(format!("{}  {}", row.label, row.detail)),
                            );
                        }
                    });
                    egui::CollapsingHeader::new(match lang {
                        Lang::Ja => "内訳（サイズ別）",
                        Lang::En => "Breakdown (by size)",
                    })
                    .id_salt("bottom_breakdown_bucket")
                    .default_open(false)
                    .show(ui, |ui| {
                        for row in &bucket_breakdown_rows {
                            ui.add(
                                egui::ProgressBar::new(row.fraction)
                                    .desired_width(260.0)
                                    .text(format!("{}  {}", row.label, row.detail)),
                            );
                        }
                    });
                }
            }
            if let Some(outcome) = &self.last_outcome {
                ui.separator();
                let clear_label = match lang {
                    Lang::Ja => "結果をクリア",
                    Lang::En => "Clear results",
                };
                if outcome.dry_run {
                    ui.horizontal(|ui| {
                        ui.label(match lang {
                            Lang::Ja => "ドライランのため削除は行われていません。",
                            Lang::En => "This was a dry run; nothing was deleted.",
                        });
                        if ui.button(clear_label).clicked() {
                            clear_outcome_clicked = true;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label(match lang {
                            Lang::Ja => format!(
                                "削除完了: 成功 {} 件、失敗 {} 件、解放 {}",
                                outcome.deleted,
                                outcome.failed,
                                view::human_size(outcome.freed_bytes)
                            ),
                            Lang::En => format!(
                                "Deletion complete: {} succeeded, {} failed, {} freed",
                                outcome.deleted,
                                outcome.failed,
                                view::human_size(outcome.freed_bytes)
                            ),
                        });
                        if ui.button(clear_label).clicked() {
                            clear_outcome_clicked = true;
                        }
                    });
                    if !outcome.failures.is_empty() {
                        let groups = view::group_failures(&outcome.failures);
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .id_salt("outcome_failures")
                            .show(ui, |ui| {
                                for (i, (message, paths)) in groups.iter().enumerate() {
                                    let header = match lang {
                                        Lang::Ja => {
                                            format!("失敗: {message}（{}件）", paths.len())
                                        }
                                        Lang::En => {
                                            format!("Failed: {message} ({} items)", paths.len())
                                        }
                                    };
                                    egui::CollapsingHeader::new(header)
                                    .id_salt(("outcome_failure_group", i))
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        egui::ScrollArea::vertical()
                                            .max_height(120.0)
                                            .id_salt(("outcome_failure_paths", i))
                                            .show(ui, |ui| {
                                                for path in paths {
                                                    ui.colored_label(
                                                        ui.visuals().error_fg_color,
                                                        path.display().to_string(),
                                                    );
                                                }
                                            });
                                    });
                                }
                            });
                    }
                }
            }
        });

        if confirm_clicked {
            self.confirming = true;
        }
        if plan_dirty {
            self.plan_dirty = true;
        }
        if clear_outcome_clicked {
            self.last_outcome = None;
        }
    }

    /// 削除実行前の最終確認。完全削除（`is_permanent`）のときは、単純な
    /// ボタン確認だけでは誤クリックに弱いため、対象件数を正確に入力しないと
    /// 「実行」ボタンが有効にならない二段階の確認にする（#26 / F-DEL-06）。
    fn ui_confirm_modal(&mut self, ctx: &egui::Context) {
        if !self.confirming {
            return;
        }
        let Some(plan) = &self.plan else {
            self.confirming = false;
            return;
        };
        let item_count = plan.item_count();
        let total_size = plan.total_size();
        let is_dry_run = plan.mode().is_dry_run();
        let is_permanent = plan.mode().method() == pc_cleaner_core::DeleteMethod::Permanent;
        let paths: Vec<String> = plan
            .items()
            .iter()
            .map(|item| item.path.display().to_string())
            .collect();

        // 内訳（C3）。`self.breakdown` / `self.buckets` は `plan` と同じ
        // タイミング（`recompute_plan`）で更新されるため、この確認モーダルが
        // 見せる `plan` の内容と一致する。以降 `confirm_text` で
        // `self.permanent_confirm_text` を可変借用するため、先に読み取っておく。
        let rule_index: HashMap<String, Rule> = self
            .rules
            .iter()
            .map(|r| (r.id.clone(), r.clone()))
            .collect();
        let lang = self.config.lang;
        let category_breakdown_rows =
            view::category_rows(&self.breakdown, &rule_index, total_size, lang);
        let bucket_breakdown_rows = view::bucket_rows(&self.buckets, total_size, lang);

        // `.open(&mut open)` は Window 側の閉じるボタン用に `open` を可変借用
        // し続けるため、本文クロージャの中で同じ `open` へ二重に可変借用は
        // できない。ボタン操作は別のローカル変数（proceed / cancel）で受け、
        // `.show()` が返った後にまとめて反映する。
        let mut open = true;
        let mut proceed = false;
        let mut cancel = false;
        let confirm_text = &mut self.permanent_confirm_text;
        let can_proceed = !is_permanent || confirm_text.trim() == item_count.to_string();

        let window_title = match lang {
            Lang::Ja => "削除の確認",
            Lang::En => "Confirm deletion",
        };
        egui::Window::new(window_title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(match lang {
                    Lang::Ja => format!(
                        "{item_count} 件（{}）を{}。",
                        view::human_size(total_size),
                        if is_permanent {
                            "完全に削除します（ゴミ箱を経由せず、復元できません）"
                        } else if is_dry_run {
                            "プレビューします（実際には削除しません）"
                        } else {
                            "ゴミ箱へ送ります"
                        }
                    ),
                    Lang::En => {
                        if is_permanent {
                            format!(
                                "Permanently delete {item_count} items ({}) (bypassing the \
                                Recycle Bin; cannot be restored).",
                                view::human_size(total_size)
                            )
                        } else if is_dry_run {
                            format!(
                                "Preview {item_count} items ({}) (nothing will actually be \
                                deleted).",
                                view::human_size(total_size)
                            )
                        } else {
                            format!(
                                "Send {item_count} items ({}) to the Recycle Bin.",
                                view::human_size(total_size)
                            )
                        }
                    }
                });
                if is_permanent {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        match lang {
                            Lang::Ja => "この操作は取り消せません。",
                            Lang::En => "This action cannot be undone.",
                        },
                    );
                }
                ui.separator();
                egui::CollapsingHeader::new(match lang {
                    Lang::Ja => "内訳（種類別）",
                    Lang::En => "Breakdown (by type)",
                })
                .id_salt("confirm_breakdown_category")
                .default_open(true)
                .show(ui, |ui| {
                    for row in &category_breakdown_rows {
                        ui.add(
                            egui::ProgressBar::new(row.fraction)
                                .desired_width(260.0)
                                .text(format!("{}  {}", row.label, row.detail)),
                        );
                    }
                });
                egui::CollapsingHeader::new(match lang {
                    Lang::Ja => "内訳（サイズ別）",
                    Lang::En => "Breakdown (by size)",
                })
                .id_salt("confirm_breakdown_bucket")
                .default_open(false)
                .show(ui, |ui| {
                    for row in &bucket_breakdown_rows {
                        ui.add(
                            egui::ProgressBar::new(row.fraction)
                                .desired_width(260.0)
                                .text(format!("{}  {}", row.label, row.detail)),
                        );
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for path in &paths {
                            ui.label(path);
                        }
                    });
                ui.separator();
                if is_permanent {
                    ui.label(match lang {
                        Lang::Ja => {
                            format!("続行するには対象件数「{item_count}」を入力してください。")
                        }
                        Lang::En => format!("To proceed, type the item count \"{item_count}\"."),
                    });
                    ui.text_edit_singleline(confirm_text);
                }
                ui.horizontal(|ui| {
                    let run_label = match lang {
                        Lang::Ja => "実行",
                        Lang::En => "Run",
                    };
                    let cancel_label = match lang {
                        Lang::Ja => "キャンセル",
                        Lang::En => "Cancel",
                    };
                    if ui
                        .add_enabled(can_proceed, egui::Button::new(run_label))
                        .clicked()
                    {
                        proceed = true;
                    }
                    if ui.button(cancel_label).clicked() {
                        cancel = true;
                    }
                });
            });

        if proceed {
            self.confirming = false;
            self.permanent_confirm_text.clear();
            let final_plan = if is_permanent {
                // permanent_unconfirmed() で作った計画は確定できないため、
                // ここで初めて permanent_confirmed() で作り直す（F-DEL-01）。
                let mode = DeleteMode::resolve(&self.config, DeleteRequest::permanent_confirmed());
                pc_cleaner_core::preview(self.platform.as_ref(), &self.entries, &self.rules, mode)
            } else if let Some(plan) = self.plan.take() {
                plan
            } else {
                return;
            };
            self.permanent = false; // 実行後は既定（ゴミ箱経由）に戻す
            self.task = Task::Deleting {
                done: 0,
                total: final_plan.item_count(),
            };
            self.rx = Some(task::spawn_delete(ctx.clone(), final_plan, self.demo));
        } else if cancel || !open {
            self.confirming = false;
            self.permanent_confirm_text.clear();
        }
    }

    /// 「管理者として実行し直す」確認モーダル（A2 / Issue #41）。
    ///
    /// `ui_confirm_modal` と同じく、ボタン操作はローカル変数
    /// （proceed / cancel）で受けてから `.show()` の後にまとめて反映する。
    ///
    /// 本 PR（A2）の時点では、昇格しても走査・削除の対象は増えない。
    /// `needs_admin` なルールを実際に対象化するのは A1（Issue #40）の責務。
    fn ui_elevate_modal(&mut self, ctx: &egui::Context) {
        if !self.confirming_elevation {
            return;
        }

        let mut open = true;
        let mut proceed = false;
        let mut cancel = false;
        let lang = self.config.lang;

        let window_title = match lang {
            Lang::Ja => "管理者として実行し直す",
            Lang::En => "Restart as administrator",
        };
        egui::Window::new(window_title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(view::elevate_confirm_text(lang));
                ui.horizontal(|ui| {
                    let run_label = match lang {
                        Lang::Ja => "実行",
                        Lang::En => "Run",
                    };
                    let cancel_label = match lang {
                        Lang::Ja => "キャンセル",
                        Lang::En => "Cancel",
                    };
                    if ui.button(run_label).clicked() {
                        proceed = true;
                    }
                    if ui.button(cancel_label).clicked() {
                        cancel = true;
                    }
                });
            });

        if proceed {
            self.confirming_elevation = false;
            // 昇格後の新プロセスは設定ファイルを読み直すため、先に保存して
            // おかないと直前の「ルール別の既定」の変更が引き継がれない。
            self.save_config();
            let argv_rest: Vec<String> = std::env::args().skip(1).collect();
            let args = view::relaunch_args(&argv_rest);
            match self.platform.elevate(&args) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(ElevateError::Cancelled) => {
                    self.notices.push(
                        match lang {
                            Lang::Ja => "管理者権限での実行はキャンセルされました。",
                            Lang::En => "Running with administrator privileges was cancelled.",
                        }
                        .to_string(),
                    );
                }
                Err(e) => {
                    self.notices.push(match lang {
                        Lang::Ja => format!("管理者権限で実行し直せませんでした: {e}"),
                        Lang::En => format!("Failed to restart as administrator: {e}"),
                    });
                }
            }
        } else if cancel || !open {
            self.confirming_elevation = false;
        }
    }

    /// 「この実行を元に戻す」確認モーダル（D3 / Issue #53）。
    ///
    /// `ui_elevate_modal` と同じく、ボタン操作はローカル変数
    /// （proceed / cancel）で受けてから `.show()` の後にまとめて反映する。
    /// 復元は削除ではないため許可リスト方式（NF-SAF-04）の対象外だが、
    /// 対象特定を誤ると意図しないファイルを動かしうるため、実行前に必ず
    /// 確認を経由させる。復元先に既存ファイルがある場合の自動上書きは
    /// `Platform::restore_from_trash` の実装側で常に禁止されている
    /// （NF-SAF-01）。
    fn ui_restore_confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(run_id) = self.confirming_restore_run_id else {
            return;
        };

        let mut open = true;
        let mut proceed = false;
        let mut cancel = false;
        let lang = self.config.lang;

        let window_title = match lang {
            Lang::Ja => "この実行を元に戻す",
            Lang::En => "Undo this run",
        };
        egui::Window::new(window_title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(match lang {
                    Lang::Ja => format!(
                        "実行 {run_id} で削除した項目のうち、ゴミ箱に残っているものを元の場所へ復元します。"
                    ),
                    Lang::En => format!(
                        "Restores items deleted in run {run_id} that are still in the \
                        Recycle Bin to their original location."
                    ),
                });
                ui.label(match lang {
                    Lang::Ja => {
                        "復元先に既に同名のファイルがある場合は、上書きせずスキップします。"
                    }
                    Lang::En => {
                        "If a file with the same name already exists at the destination, it \
                        will be skipped rather than overwritten."
                    }
                });
                ui.horizontal(|ui| {
                    let restore_label = match lang {
                        Lang::Ja => "復元",
                        Lang::En => "Restore",
                    };
                    let cancel_label = match lang {
                        Lang::Ja => "キャンセル",
                        Lang::En => "Cancel",
                    };
                    if ui.button(restore_label).clicked() {
                        proceed = true;
                    }
                    if ui.button(cancel_label).clicked() {
                        cancel = true;
                    }
                });
            });

        if proceed {
            self.confirming_restore_run_id = None;
            self.last_restore_outcome = None;
            self.task = Task::Restoring;
            self.rx = Some(task::spawn_restore(ctx.clone(), run_id, self.demo));
        } else if cancel || !open {
            self.confirming_restore_run_id = None;
        }
    }
}

fn pref_label(pref: pc_cleaner_core::RulePref, lang: Lang) -> &'static str {
    match (pref, lang) {
        (pc_cleaner_core::RulePref::AlwaysSelect, Lang::Ja) => "常に選択",
        (pc_cleaner_core::RulePref::Exclude, Lang::Ja) => "常に除外",
        (pc_cleaner_core::RulePref::AskEachTime, Lang::Ja) => "毎回確認",
        (pc_cleaner_core::RulePref::AlwaysSelect, Lang::En) => "Always select",
        (pc_cleaner_core::RulePref::Exclude, Lang::En) => "Always exclude",
        (pc_cleaner_core::RulePref::AskEachTime, Lang::En) => "Ask each time",
    }
}
