//! アプリケーション状態と `eframe::App` 実装。
//!
//! `core` の呼び出しと表示・操作のみを行い、判定・削除ロジックは持たない
//! （F-GUI-07）。走査・推奨の手順は CLI（`cli/src/main.rs` の
//! `scan_and_apply_prefs` / `run_clean`）と同一の core 呼び出し列に揃える
//! こと（F-CLI-08 / 9.5）。変更する場合は両方を直すこと。

use eframe::egui;
use pc_cleaner_core::platform::Platform;
use pc_cleaner_core::{
    Config, DeleteMode, DeleteOutcome, DeletePlan, DeleteProgress, DeleteRequest, ElevateError,
    ItemOutcome, Rule, ScanEntry, ScanProgress, SkipReason, config, rule,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

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
    /// 現在のプロセスが管理者権限で動作しているか（起動時に一度だけ判定し
    /// 保持する。実行中に変化しないため毎フレーム問い合わせる必要はない）。
    elevated: bool,
    /// 「管理者として実行し直す」確認モーダルを表示中か。
    confirming_elevation: bool,

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

        let mut notices = Vec::new();
        if relaunched && !elevated {
            // should_relaunch の二重防御が効いた場合。通常は起こらないが、
            // 起きた場合は静かに非昇格のまま続けるのではなく理由を伝える。
            notices.push(
                "管理者権限で起動し直しましたが、昇格を確認できませんでした。\
                 管理者権限が必要な領域は対象外のままです。"
                    .to_string(),
            );
        }

        App {
            platform,
            config_path,
            config,
            demo,
            elevated,
            confirming_elevation: false,
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
            permanent: false,
            permanent_confirm_text: String::new(),
            confirming: false,
            last_outcome: None,
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
        self.ui_elevate_modal(ctx);
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
                    ui.separator();
                    if self.elevated {
                        ui.label("🔓 管理者権限で実行中");
                    } else {
                        ui.add_enabled_ui(!self.is_busy(), |ui| {
                            if ui
                                .button("🔒 管理者として実行し直す")
                                .on_hover_text(view::elevate_confirm_text())
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
        let frame = egui::Frame::side_top_panel(&ctx.style()).fill(CHROME_BG);
        egui::SidePanel::left("rules").frame(frame).show(ctx, |ui| {
            ui.heading("ルール別の既定");
            ui.label("常に選択 / 除外 / 毎回確認(推奨に従う)を設定できます。");
            ui.separator();

            let mut changed_rule: Option<String> = None;
            let mut changed_threshold_rule_id: Option<String> = None;
            // `.show()` に渡すクロージャ内で `&mut self.config` を書き換えつつ
            // `self.config` を読んで作った `Vec<Rule>` を同時に借用すると
            // 競合するため、先にルール一覧をローカル変数へ取り出しておく
            // （app.rs 内の他の `.show()` 呼び出しと同じパターン）。
            let scannable_rules = rule::scannable_rules(&self.config, self.elevated);
            for rule in &scannable_rules {
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
                if let Some(default_days) = rule.age_threshold_days {
                    let mut days = self.config.age_threshold_days(&rule.id, default_days);
                    ui.horizontal(|ui| {
                        ui.label("しきい値（日）:");
                        let resp = ui.add(egui::DragValue::new(&mut days).range(
                            pc_cleaner_core::config::AGE_THRESHOLD_MIN_DAYS
                                ..=pc_cleaner_core::config::AGE_THRESHOLD_MAX_DAYS,
                        ));
                        if resp.drag_stopped() || resp.lost_focus() {
                            self.config.age_thresholds.insert(rule.id.clone(), days);
                            changed_threshold_rule_id = Some(rule.id.clone());
                        }
                    });
                    ui.small("変更すると再走査します。");
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

            let future_rules = view::future_rules(&self.config, self.elevated);
            if !future_rules.is_empty() {
                ui.separator();
                ui.heading("管理者権限が必要（未対応）");
                for rule in &future_rules {
                    ui.label(format!("🔒 {}", rule.label))
                        .on_hover_text(&rule.description);
                }
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

        let frame = egui::Frame::central_panel(&ctx.style()).fill(egui::Color32::WHITE);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
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
                            // 「安全」も注意/要確認と同じく意味を持つ色で示す。
                            // 既定の weak_text_color は意図的に低コントラストで
                            // 読みづらいため、明示的な緑を使う。
                            const SAFE_COLOR: egui::Color32 = egui::Color32::from_rgb(21, 115, 71);
                            let color = match safety {
                                pc_cleaner_core::Safety::Safe => SAFE_COLOR,
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
                    });
                    // パスは行内に置くと横幅を超えて見切れるため、独立した行に
                    // 出して CentralPanel の幅に収める。
                    ui.small(entry.path.display().to_string());
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
        let mut plan_dirty = false;
        let mut clear_outcome_clicked = false;

        let frame = egui::Frame::side_top_panel(&ctx.style()).fill(CHROME_BG);
        egui::TopBottomPanel::bottom("bottom")
            .frame(frame)
            .show(ctx, |ui| {
            if let Some((item_count, total_size, is_empty, excluded_len, needs_permanent)) =
                plan_summary
            {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "選択中: {item_count} 件（{}）を解放",
                        view::human_size(total_size)
                    ));
                    if ui
                        .checkbox(&mut self.dry_run, "削除せず確認だけ（ドライラン）")
                        .changed()
                    {
                        plan_dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.permanent,
                            "完全削除（ゴミ箱を経由しない、復旧不可）",
                        )
                        .changed()
                    {
                        plan_dirty = true;
                    }
                    if ui
                        .add_enabled(!busy && !is_empty, egui::Button::new("確定"))
                        .clicked()
                    {
                        confirm_clicked = true;
                    }
                });
                if self.permanent {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "完全削除は復元できません。確定時にあらためて確認します。",
                    );
                }
                if excluded_len > 0 {
                    ui.small(format!(
                        "除外: {excluded_len} 件（選択されていない・許可リスト外・要管理者権限など）"
                    ));
                    if needs_permanent && !self.permanent {
                        ui.small(view::recycle_bin_exclusion_hint());
                    }
                }
            }
            if let Some(outcome) = &self.last_outcome {
                ui.separator();
                if outcome.dry_run {
                    ui.horizontal(|ui| {
                        ui.label("ドライランのため削除は行われていません。");
                        if ui.button("結果をクリア").clicked() {
                            clear_outcome_clicked = true;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "削除完了: 成功 {} 件、失敗 {} 件、解放 {}",
                            outcome.deleted,
                            outcome.failed,
                            view::human_size(outcome.freed_bytes)
                        ));
                        if ui.button("結果をクリア").clicked() {
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
                                    egui::CollapsingHeader::new(format!(
                                        "失敗: {message}（{}件）",
                                        paths.len()
                                    ))
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

        // `.open(&mut open)` は Window 側の閉じるボタン用に `open` を可変借用
        // し続けるため、本文クロージャの中で同じ `open` へ二重に可変借用は
        // できない。ボタン操作は別のローカル変数（proceed / cancel）で受け、
        // `.show()` が返った後にまとめて反映する。
        let mut open = true;
        let mut proceed = false;
        let mut cancel = false;
        let confirm_text = &mut self.permanent_confirm_text;
        let can_proceed = !is_permanent || confirm_text.trim() == item_count.to_string();

        egui::Window::new("削除の確認")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!(
                    "{item_count} 件（{}）を{}。",
                    view::human_size(total_size),
                    if is_permanent {
                        "完全に削除します（ゴミ箱を経由せず、復元できません）"
                    } else if is_dry_run {
                        "プレビューします（実際には削除しません）"
                    } else {
                        "ゴミ箱へ送ります"
                    }
                ));
                if is_permanent {
                    ui.colored_label(ui.visuals().error_fg_color, "この操作は取り消せません。");
                }
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
                    ui.label(format!(
                        "続行するには対象件数「{item_count}」を入力してください。"
                    ));
                    ui.text_edit_singleline(confirm_text);
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(can_proceed, egui::Button::new("実行"))
                        .clicked()
                    {
                        proceed = true;
                    }
                    if ui.button("キャンセル").clicked() {
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

        egui::Window::new("管理者として実行し直す")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(view::elevate_confirm_text());
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
            self.confirming_elevation = false;
            // 昇格後の新プロセスは設定ファイルを読み直すため、先に保存して
            // おかないと直前の「ルール別の既定」の変更が引き継がれない。
            self.save_config();
            let argv_rest: Vec<String> = std::env::args().skip(1).collect();
            let args = view::relaunch_args(&argv_rest);
            match self.platform.elevate(&args) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(ElevateError::Cancelled) => {
                    self.notices
                        .push("管理者権限での実行はキャンセルされました。".to_string());
                }
                Err(e) => {
                    self.notices
                        .push(format!("管理者権限で実行し直せませんでした: {e}"));
                }
            }
        } else if cancel || !open {
            self.confirming_elevation = false;
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
