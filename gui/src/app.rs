//! アプリケーション状態と `eframe::App` 実装。
//!
//! `core` の呼び出しと表示・操作のみを行い、判定・削除ロジックは持たない
//! （F-GUI-07）。走査・推奨の手順は CLI（`cli/src/main.rs` の
//! `scan_and_apply_prefs` / `run_clean`）と同一の core 呼び出し列に揃える
//! こと（F-CLI-08 / 9.5）。変更する場合は両方を直すこと。

use eframe::egui;
use pc_cleaner_core::platform::Platform;
use pc_cleaner_core::{
    Config, DeleteMode, DeleteOutcome, DeletePlan, DeleteProgress, DeleteRequest, ItemOutcome,
    Rule, ScanEntry, ScanProgress, SkipReason, config, rule,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::task::{self, WorkerMsg};
use crate::view::{self, Scope};

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

/// pc-cleaner の GUI 本体。
pub struct App {
    platform: Box<dyn Platform>,
    config_path: Option<PathBuf>,
    config: Config,
    demo: bool,

    scope: Scope,
    rules: Vec<Rule>,
    entries: Vec<ScanEntry>,
    skipped: Vec<(String, SkipReason)>,

    plan: Option<DeletePlan>,
    plan_dirty: bool,
    exclusion_map: HashMap<PathBuf, &'static str>,

    task: Task,
    rx: Option<Receiver<WorkerMsg>>,

    dry_run: bool,
    confirming: bool,
    last_outcome: Option<OutcomeSummary>,
    notices: Vec<String>,
    started: bool,
}

impl App {
    /// `demo` が `true` の場合、`demo` feature が有効ならサンドボックスの
    /// `Platform` を使う（開発時の目視確認用）。
    pub fn new(demo: bool) -> Self {
        let platform = task::make_platform(demo);
        let config_path = config::config_file_path(platform.as_ref());
        let config = config_path.as_deref().map(config::load).unwrap_or_default();

        App {
            platform,
            config_path,
            config,
            demo,
            scope: Scope::SafeOnly,
            rules: Vec::new(),
            entries: Vec::new(),
            skipped: Vec::new(),
            plan: None,
            plan_dirty: true,
            exclusion_map: HashMap::new(),
            task: Task::Idle,
            rx: None,
            dry_run: false,
            confirming: false,
            last_outcome: None,
            notices: Vec::new(),
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
                WorkerMsg::ScanDone { rules, entries } => {
                    self.rules = rules;
                    self.entries = entries;
                    self.task = Task::Idle;
                    self.plan_dirty = true;
                    keep_receiver = false;
                }
                WorkerMsg::Delete(DeleteProgress::ItemFinished { done, total, .. }) => {
                    self.task = Task::Deleting { done, total };
                }
                WorkerMsg::Delete(_) => {}
                WorkerMsg::DeleteDone(outcome) => {
                    self.last_outcome = Some(outcome.into());
                    self.task = Task::Idle;
                    // 削除済みエントリが一覧に残らないよう自動で再走査する。
                    self.entries.clear();
                    self.skipped.clear();
                    self.plan = None;
                    self.plan_dirty = true;
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
    fn recompute_plan(&mut self) {
        let request = if self.dry_run {
            DeleteRequest::dry_run()
        } else {
            DeleteRequest::execute()
        };
        let mode = DeleteMode::resolve(&self.config, request);
        let plan =
            pc_cleaner_core::preview(self.platform.as_ref(), &self.entries, &self.rules, mode);
        self.exclusion_map =
            view::to_exclusion_map(plan.excluded().iter().map(|e| (e.path.clone(), e.reason)));
        self.plan = Some(plan);
        self.plan_dirty = false;
    }

    fn save_config(&mut self) {
        if let Some(path) = &self.config_path {
            if let Err(e) = config::save(&self.config, path) {
                self.notices.push(format!("設定の保存に失敗しました: {e}"));
            }
        } else {
            self.notices
                .push("この環境では設定の保存先を特定できません。".to_string());
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
    }
}

impl App {
    fn ui_top(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("pc-cleaner");
                ui.separator();
                let busy = self.is_busy();
                ui.add_enabled_ui(!busy, |ui| {
                    let mut changed = false;
                    for scope in [Scope::SafeOnly, Scope::All] {
                        if ui
                            .radio_value(&mut self.scope, scope, scope.label())
                            .changed()
                        {
                            changed = true;
                        }
                    }
                    if ui.button("再走査").clicked() {
                        changed = true;
                    }
                    if changed {
                        self.start_scan(ctx);
                    }
                });
                match &self.task {
                    Task::Scanning {
                        rule_id,
                        files_scanned,
                    } => {
                        ui.spinner();
                        ui.label(format!(
                            "走査中: {} … {files_scanned} 件",
                            rule_id.as_deref().unwrap_or("準備中")
                        ));
                    }
                    Task::Deleting { done, total } => {
                        ui.spinner();
                        ui.label(format!("削除中: {done} / {total} 件"));
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
        egui::SidePanel::left("rules").show(ctx, |ui| {
            ui.heading("ルール別の既定");
            ui.label("常に選択 / 除外 / 毎回確認(推奨に従う)を設定できます。");
            ui.separator();

            let mut changed_rule: Option<String> = None;
            for rule in rule::scannable_rules() {
                ui.label(&rule.label).on_hover_text(&rule.description);
                let mut pref = self
                    .config
                    .rule_prefs
                    .get(&rule.id)
                    .copied()
                    .unwrap_or_default();
                egui::ComboBox::from_id_salt(&rule.id)
                    .selected_text(pref_label(pref))
                    .show_ui(ui, |ui| {
                        for candidate in [
                            pc_cleaner_core::RulePref::AskEachTime,
                            pc_cleaner_core::RulePref::AlwaysSelect,
                            pc_cleaner_core::RulePref::Exclude,
                        ] {
                            if ui
                                .selectable_value(&mut pref, candidate, pref_label(candidate))
                                .changed()
                            {
                                self.config.rule_prefs.insert(rule.id.clone(), pref);
                                changed_rule = Some(rule.id.clone());
                            }
                        }
                    });
                ui.add_space(4.0);
            }
            if let Some(rule_id) = changed_rule {
                view::reapply_pref_for_rule(&mut self.entries, &rule_id, &self.config);
                self.plan_dirty = true;
                self.save_config();
            }

            ui.separator();
            ui.checkbox(&mut self.config.use_trash, "ゴミ箱経由で削除する")
                .on_hover_text("オフにすると通常の削除は実行できず、プレビューのみになります。");
            if ui
                .checkbox(&mut self.config.dry_run_default, "既定でドライラン")
                .changed()
            {
                self.plan_dirty = true;
            }
            if ui.button("設定を保存").clicked() {
                self.save_config();
            }
            if let Some(path) = &self.config_path {
                ui.small(format!("保存先: {}", path.display()));
            }

            ui.separator();
            ui.heading("将来対応（管理者権限が必要）");
            for rule in view::future_rules() {
                ui.label(format!("🔒 {}", rule.label))
                    .on_hover_text(&rule.description);
            }

            if !self.skipped.is_empty() {
                ui.separator();
                ui.heading("スキップされたルール");
                for (rule_id, reason) in &self.skipped {
                    ui.label(format!("{rule_id}: {}", view::skip_reason_label(reason)));
                }
            }
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

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if entries.is_empty() && !busy {
                    ui.label("対象なし。");
                }
                for entry in &mut entries {
                    let rule = rule_index.get(&entry.rule_id);
                    let safety = rule.map(|r| r.safety);
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut entry.selected, "").changed() {
                            plan_dirty = true;
                        }
                        if let Some(safety) = safety {
                            let color = match safety {
                                pc_cleaner_core::Safety::Safe => ui.visuals().weak_text_color(),
                                pc_cleaner_core::Safety::Caution => ui.visuals().warn_fg_color,
                                pc_cleaner_core::Safety::Review => ui.visuals().error_fg_color,
                            };
                            ui.colored_label(color, view::safety_label(safety));
                        }
                        ui.label(rule.map(|r| r.label.as_str()).unwrap_or(&entry.rule_id));
                        ui.label(view::human_size(entry.size));
                        ui.label(view::age_label(entry.age_days));
                        if let Some(hint) = exclusion_map.get(&entry.path) {
                            ui.colored_label(ui.visuals().warn_fg_color, *hint);
                        }
                        ui.label(entry.path.display().to_string())
                            .on_hover_text(entry.path.display().to_string());
                    });
                    if !entry.reason.is_empty() {
                        ui.small(format!("理由: {}", entry.reason));
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

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            if let Some((item_count, total_size, is_empty, excluded_len, needs_permanent)) =
                plan_summary
            {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "選択中: {item_count} 件（{}）を解放",
                        view::human_size(total_size)
                    ));
                    ui.checkbox(&mut self.dry_run, "削除せず確認だけ（ドライラン）");
                    if ui
                        .add_enabled(!busy && !is_empty, egui::Button::new("確定"))
                        .clicked()
                    {
                        confirm_clicked = true;
                    }
                });
                if excluded_len > 0 {
                    ui.small(format!(
                        "除外: {excluded_len} 件（選択されていない・許可リスト外・要管理者権限など）"
                    ));
                    if needs_permanent {
                        ui.small(view::recycle_bin_exclusion_hint());
                    }
                }
            }
            if let Some(outcome) = &self.last_outcome {
                ui.separator();
                if outcome.dry_run {
                    ui.label("ドライランのため削除は行われていません。");
                } else {
                    ui.label(format!(
                        "削除完了: 成功 {} 件、失敗 {} 件、解放 {}",
                        outcome.deleted,
                        outcome.failed,
                        view::human_size(outcome.freed_bytes)
                    ));
                    for (path, message) in &outcome.failures {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            format!("失敗: {} — {message}", path.display()),
                        );
                    }
                }
            }
        });

        if confirm_clicked {
            self.confirming = true;
        }
    }

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
        let paths: Vec<String> = plan
            .items()
            .iter()
            .map(|item| item.path.display().to_string())
            .collect();

        // `.open(&mut open)` は Window 側の閉じるボタン用に `open` を可変借用
        // し続けるため、本文クロージャの中で同じ `open` へ二重に可変借用は
        // できない。ボタン操作は別のローカル変数（proceed / cancel）で受け、
        // `.show()` が返った後にまとめて反映する。
        let mut open = true;
        let mut proceed = false;
        let mut cancel = false;
        egui::Window::new("削除の確認")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!(
                    "{item_count} 件（{}）を{}。",
                    view::human_size(total_size),
                    if is_dry_run {
                        "プレビューします（実際には削除しません）"
                    } else {
                        "ゴミ箱へ送ります"
                    }
                ));
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for path in &paths {
                            ui.label(path);
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("実行").clicked() {
                        proceed = true;
                    }
                    if ui.button("キャンセル").clicked() {
                        cancel = true;
                    }
                });
            });

        if proceed {
            self.confirming = false;
            if let Some(plan) = self.plan.take() {
                self.task = Task::Deleting {
                    done: 0,
                    total: plan.item_count(),
                };
                self.rx = Some(task::spawn_delete(ctx.clone(), plan, self.demo));
            }
        } else if cancel || !open {
            self.confirming = false;
        }
    }
}

fn pref_label(pref: pc_cleaner_core::RulePref) -> &'static str {
    match pref {
        pc_cleaner_core::RulePref::AlwaysSelect => "常に選択",
        pc_cleaner_core::RulePref::Exclude => "常に除外",
        pc_cleaner_core::RulePref::AskEachTime => "毎回確認",
    }
}
