//! 走査機能（scan）。有効なルールの基点配下を走査し、[`ScanEntry`] として
//! 集計する。F-SCAN-01〜08。
//!
//! `ScanEntry` の粒度はルールの [`MatchKind`] によって変える：
//! - `All` は基点直下の子（トップレベル）ごとに1エントリ（子がディレクトリ
//!   なら配下を再帰集計）。基点全体を1エントリにすると `to_trash(entry.path)`
//!   が基点フォルダごと送る操作になってしまい、削除単位として粗すぎる。
//! - `Extension` / `OlderThan` はマッチした個々のファイルごとに1エントリ。
//!   ディレクトリ単位で集約すると、マッチしないファイルまで削除候補の一部に
//!   巻き込み F-SCAN-08（許可リスト方式）に違反するため。

use crate::config::{Config, RulePref};
use crate::entry::ScanEntry;
use crate::platform::Platform;
use crate::recommend::recommend;
use crate::rule::{MatchKind, Rule, Safety};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::SystemTime;

/// 1日の秒数。
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// ディレクトリ走査の深さ上限。ジャンクション等による暴走を防ぐ防御的措置。
const MAX_DEPTH: usize = 32;

/// 進捗通知（[`ScanProgress::Walking`]）の間引き間隔（ファイル数）。
const PROGRESS_FILE_INTERVAL: u64 = 512;

/// ルールが走査対象から除外された理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// 管理者権限が必要なルール、または基点が管理者権限領域と判定された
    /// （F-SCAN-04 / NF-SAF-05）。
    NeedsAdmin,
    /// `Platform::known_dir` が基点を解決できなかった。
    UnknownBase,
    /// 基点ディレクトリが読み取れなかった。
    Unreadable,
    /// `MatchKind::OlderThan` なのに `age_threshold_days` が未設定だった
    /// （ルール定義の不備。全件を候補にする誤動作を避けるため走査自体を止める）。
    MissingThreshold,
}

/// 走査の進捗イベント。UI 非依存（NF-MNT-01 / NF-PRF-02）。
///
/// 保証するのは「`Started` が最初、`Finished` が最後」であることのみ。
/// ルール間の走査は並列実行されるため、それ以外の順序は非決定的である。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanProgress {
    /// 走査を開始した。
    Started {
        /// 走査対象として選ばれたルール数（スキップされたものを除く）。
        total_rules: usize,
    },
    /// ルールが走査対象から除外された。
    RuleSkipped {
        /// 除外されたルールの `id`。
        rule_id: String,
        /// 除外理由。
        reason: SkipReason,
    },
    /// ルールの走査を開始した。
    RuleStarted {
        /// ルールの `id`。
        rule_id: String,
        /// 解決された基点の実パス。
        base: PathBuf,
    },
    /// ルールの走査中の途中経過（一定件数ごと）。
    Walking {
        /// ルールの `id`。
        rule_id: String,
        /// ここまでに走査したファイル数。
        files_scanned: u64,
        /// ここまでに候補となったファイルの合計バイト数。
        bytes_scanned: u64,
    },
    /// ルールの走査が完了した。
    RuleFinished {
        /// ルールの `id`。
        rule_id: String,
        /// 生成された `ScanEntry` 数。
        entries: usize,
        /// 生成された `ScanEntry` の合計サイズ（バイト）。
        total_size: u64,
    },
    /// 全ルールの走査が完了した。
    Finished {
        /// 生成された `ScanEntry` の総数。
        entries: usize,
        /// 合計サイズ（バイト）。
        total_size: u64,
    },
}

/// 指定した安全度のルールだけを返す。`needs_admin` なルールは常に除外する
/// （F-SCAN-04）。
///
/// フロー①（ワンクリック掃除）は `rules_for_safeties(&[Safety::Safe])`、
/// フロー②（手動レビュー）は
/// `rules_for_safeties(&[Safety::Safe, Safety::Caution, Safety::Review])` と
/// 呼ぶことで、F-SCAN-06 / F-SCAN-07 を CLI/GUI 共通の同一 API で表現する。
pub fn rules_for_safeties(safeties: &[Safety]) -> Vec<Rule> {
    crate::rule::scannable_rules()
        .into_iter()
        .filter(|r| safeties.contains(&r.safety))
        .collect()
}

/// F-SCAN-06 / F-SCAN-07 の標準スコープを返す。`all == false` は `Safe` の
/// みでフロー①（ワンクリック掃除）、`all == true` は `Safe` / `Caution` /
/// `Review` すべてでフロー②（手動レビュー）に対応する。
///
/// CLI の `--all` フラグ・GUI のスコープ切替はいずれもこの1関数を経由させ、
/// `Vec<Safety>` の組み立てを重複させない（F-CLI-08 / 9.5）。
pub fn safety_scope(all: bool) -> Vec<Safety> {
    if all {
        vec![Safety::Safe, Safety::Caution, Safety::Review]
    } else {
        vec![Safety::Safe]
    }
}

/// `config.rule_prefs` を走査結果の `selected` に適用する純粋関数（F-CFG-01）。
///
/// `AlwaysSelect` は強制的に選択、`Exclude` は強制的に除外、`AskEachTime`
/// （未設定時も同様）は `recommend()` が設定した既定（`entry.recommended`）を
/// そのまま使う。
pub fn apply_rule_prefs(entries: &mut [ScanEntry], config: &Config) {
    for entry in entries {
        entry.selected = match config.rule_prefs.get(&entry.rule_id) {
            Some(RulePref::AlwaysSelect) => true,
            Some(RulePref::Exclude) => false,
            Some(RulePref::AskEachTime) | None => entry.recommended,
        };
    }
}

/// `rules` を走査して `ScanEntry` を返す。進捗通知は行わない。
pub fn scan(platform: &dyn Platform, rules: &[Rule]) -> Vec<ScanEntry> {
    scan_with_progress(platform, rules, |_| {})
}

/// [`scan`] してから [`apply_rule_prefs`] を適用するまでの一連の処理。
///
/// CLI（`scan_and_apply_prefs`）・GUI（走査ワーカー）はいずれもこの2関数を
/// 同じ順序で呼んでいた。CLI/GUI にロジックを持たせない（F-CLI-01 /
/// F-GUI-07）という原則をこの1関数に体現し、両者から呼び出す。
pub fn scan_pipeline(platform: &dyn Platform, rules: &[Rule], config: &Config) -> Vec<ScanEntry> {
    let mut entries = scan(platform, rules);
    apply_rule_prefs(&mut entries, config);
    entries
}

/// [`scan_pipeline`] の進捗通知つき版（GUI の非同期走査で使う）。
pub fn scan_pipeline_with_progress(
    platform: &dyn Platform,
    rules: &[Rule],
    config: &Config,
    on_progress: impl FnMut(ScanProgress),
) -> Vec<ScanEntry> {
    let mut entries = scan_with_progress(platform, rules, on_progress);
    apply_rule_prefs(&mut entries, config);
    entries
}

/// `rules` を走査して `ScanEntry` を返す。`on_progress` で進捗を通知する。
///
/// エラーを返さない：解決できない基点・読み取れないディレクトリ・取得できない
/// メタデータは、該当するルール／ファイルを候補から落とすだけで走査全体を
/// 失敗させない（NF-SAF-01 に寄せた安全側の挙動）。`on_progress` は常に
/// 呼び出し元スレッドから呼ばれる（UI の非 `Send` な状態をそのまま掴める）。
pub fn scan_with_progress(
    platform: &dyn Platform,
    rules: &[Rule],
    mut on_progress: impl FnMut(ScanProgress),
) -> Vec<ScanEntry> {
    let now = SystemTime::now();

    // ---- 解決フェーズ（呼び出し元スレッド・逐次） ----
    let mut targets = Vec::new();
    for rule in rules {
        match resolve_target(platform, rule) {
            Ok(target) => targets.push(target),
            Err(reason) => on_progress(ScanProgress::RuleSkipped {
                rule_id: rule.id.clone(),
                reason,
            }),
        }
    }

    on_progress(ScanProgress::Started {
        total_rules: targets.len(),
    });

    // ---- 走査フェーズ（基点ごとに並列。Platform は参照しない） ----
    let (tx, rx) = mpsc::channel::<ScanProgress>();
    let results: Vec<Vec<ScanEntry>> = thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|target| {
                let tx = tx.clone();
                scope.spawn(move || walk_target(target, now, &tx))
            })
            .collect();
        drop(tx);
        for event in rx {
            on_progress(event);
        }
        // targets の順に join することで、完了順ではなく入力ルール順の
        // 決定的な結果順序を保証する（9.5：CLI/GUI で結果が一致すること）。
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    // ---- 集約フェーズ ----
    let entries: Vec<ScanEntry> = results.into_iter().flatten().collect();

    on_progress(ScanProgress::Finished {
        entries: entries.len(),
        total_size: entries.iter().map(|e| e.size).sum(),
    });

    entries
}

/// 走査対象として解決されたルールと、その実パス。
struct ScanTarget<'a> {
    rule: &'a Rule,
    base: PathBuf,
}

/// `rule` を走査対象にできるか判定し、できるなら基点を解決する。
///
/// 呼び出し側は通常 [`rules_for_safeties`]（内部で `scannable_rules()` を
/// 使う）で `needs_admin` なルールを渡さないが、誤って渡された場合の
/// 二次防御としてここでも再チェックする（`recommend()` が同様の再判定を
/// 行っているのに倣う）。
fn resolve_target<'a>(
    platform: &dyn Platform,
    rule: &'a Rule,
) -> Result<ScanTarget<'a>, SkipReason> {
    if rule.needs_admin {
        return Err(SkipReason::NeedsAdmin);
    }
    if matches!(rule.match_kind, MatchKind::OlderThan) && rule.age_threshold_days.is_none() {
        return Err(SkipReason::MissingThreshold);
    }
    let base = platform
        .known_dir(rule.base)
        .ok_or(SkipReason::UnknownBase)?;
    if platform.requires_admin(&base) {
        return Err(SkipReason::NeedsAdmin);
    }
    Ok(ScanTarget { rule, base })
}

/// 1つの走査対象を処理し、`ScanEntry` を生成する（並列実行されるワーカー）。
///
/// `recommend()` を適用して `recommended` / `reason` / `selected` を埋める
/// （`Config` による上書きは [`apply_rule_prefs`] が呼び出し側で行う）。
fn walk_target(
    target: &ScanTarget,
    now: SystemTime,
    tx: &mpsc::Sender<ScanProgress>,
) -> Vec<ScanEntry> {
    let rule = target.rule;
    let _ = tx.send(ScanProgress::RuleStarted {
        rule_id: rule.id.clone(),
        base: target.base.clone(),
    });

    let mut entries = match &rule.match_kind {
        MatchKind::All => walk_all(&target.base, &rule.id, now, tx),
        MatchKind::Extension(extensions) => {
            walk_matched_files(&target.base, &rule.id, now, tx, |path, _metadata| {
                matches_extension(path, extensions)
            })
        }
        MatchKind::OlderThan => {
            // resolve_target が Some を保証している。
            let threshold = rule.age_threshold_days.unwrap_or(0);
            walk_matched_files(&target.base, &rule.id, now, tx, |_path, metadata| {
                metadata
                    .modified()
                    .ok()
                    .map(|modified| age_days_between(modified, now))
                    .is_some_and(|age| age >= threshold)
            })
        }
    };

    for entry in &mut entries {
        let recommendation = recommend(entry, rule);
        entry.recommended = recommendation.recommended;
        entry.selected = recommendation.recommended;
        entry.reason = recommendation.reason;
    }

    // read_dir の返す順序は OS 依存のため、再実行間・CLI/GUI 間で結果が
    // 一致するよう path でソートする（9.5）。
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let total_size: u64 = entries.iter().map(|e| e.size).sum();
    let _ = tx.send(ScanProgress::RuleFinished {
        rule_id: rule.id.clone(),
        entries: entries.len(),
        total_size,
    });

    entries
}

/// `MatchKind::All`：`base` 直下の子ごとに1エントリを生成する。
/// 子がディレクトリならその配下を再帰集計し、ファイル数0のディレクトリ
/// （空・読み取り不能）はノイズになるため候補に含めない。
fn walk_all(
    base: &Path,
    rule_id: &str,
    now: SystemTime,
    tx: &mpsc::Sender<ScanProgress>,
) -> Vec<ScanEntry> {
    let mut entries = Vec::new();
    let Ok(read_dir) = fs::read_dir(base) else {
        return entries;
    };

    let mut files_scanned = 0u64;
    let mut bytes_scanned = 0u64;

    for child in read_dir.flatten() {
        let path = child.path();
        let Ok(file_type) = child.file_type() else {
            continue;
        };

        let new_entry = if file_type.is_symlink() {
            // シンボリックリンク・リパースポイントは辿らない。葉として扱う。
            let size = fs::symlink_metadata(&path).map(|m| m.len()).unwrap_or(0);
            Some(build_entry(rule_id, path, size, 1, None, now))
        } else if file_type.is_dir() {
            let (size, count, modified) = aggregate_dir(&path);
            (count > 0).then(|| build_entry(rule_id, path, size, count, modified, now))
        } else if file_type.is_file() {
            child.metadata().ok().map(|metadata| {
                build_entry(
                    rule_id,
                    path,
                    metadata.len(),
                    1,
                    metadata.modified().ok(),
                    now,
                )
            })
        } else {
            None
        };

        if let Some(entry) = new_entry {
            files_scanned += 1;
            bytes_scanned += entry.size;
            entries.push(entry);
            report_progress(tx, rule_id, files_scanned, bytes_scanned);
        }
    }

    entries
}

/// `Extension` / `OlderThan`：`base` 配下を再帰し、`is_match` に合致する
/// ファイルごとに1エントリを生成する。
fn walk_matched_files(
    base: &Path,
    rule_id: &str,
    now: SystemTime,
    tx: &mpsc::Sender<ScanProgress>,
    is_match: impl Fn(&Path, &fs::Metadata) -> bool,
) -> Vec<ScanEntry> {
    let mut entries = Vec::new();
    let mut files_scanned = 0u64;
    let mut bytes_scanned = 0u64;

    walk_files(base, |path, metadata| {
        files_scanned += 1;
        if is_match(path, metadata) {
            let entry = build_entry(
                rule_id,
                path.to_path_buf(),
                metadata.len(),
                1,
                metadata.modified().ok(),
                now,
            );
            bytes_scanned += entry.size;
            entries.push(entry);
        }
        report_progress(tx, rule_id, files_scanned, bytes_scanned);
    });

    entries
}

/// `PROGRESS_FILE_INTERVAL` 件ごとに [`ScanProgress::Walking`] を送る。
fn report_progress(
    tx: &mpsc::Sender<ScanProgress>,
    rule_id: &str,
    files_scanned: u64,
    bytes_scanned: u64,
) {
    if files_scanned % PROGRESS_FILE_INTERVAL == 0 {
        let _ = tx.send(ScanProgress::Walking {
            rule_id: rule_id.to_string(),
            files_scanned,
            bytes_scanned,
        });
    }
}

/// `root` 配下（自身を含む）を反復的に走査し、ファイルを見つけるたびに
/// `visit_file` を呼ぶ。再帰呼び出しにしないのは深い階層でのスタック
/// オーバーフローを避けるため。シンボリックリンク・リパースポイントは
/// 辿らない（ループ防止・F-SCAN-08 の担保）。`MAX_DEPTH` を超えたサブツリー
/// は打ち切る。読み取れないディレクトリ・取得できないメタデータはスキップ
/// する（NF-SAF-01 寄りの安全側）。
fn walk_files(root: &Path, mut visit_file: impl FnMut(&Path, &fs::Metadata)) {
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            } else if file_type.is_dir() {
                stack.push((path, depth + 1));
            } else if file_type.is_file() {
                if let Ok(metadata) = entry.metadata() {
                    visit_file(&path, &metadata);
                }
            }
        }
    }
}

/// `root` 配下（自身を含む）の合計サイズ・ファイル数・最新更新日時を集計する。
fn aggregate_dir(root: &Path) -> (u64, u64, Option<SystemTime>) {
    let mut total_size = 0u64;
    let mut file_count = 0u64;
    let mut latest: Option<SystemTime> = None;

    walk_files(root, |_path, metadata| {
        total_size += metadata.len();
        file_count += 1;
        if let Ok(modified) = metadata.modified() {
            latest = Some(match latest {
                Some(prev) if prev >= modified => prev,
                _ => modified,
            });
        }
    });

    (total_size, file_count, latest)
}

/// `ScanEntry` を組み立てる。`age_days` は `modified` と `now` から算出する。
fn build_entry(
    rule_id: &str,
    path: PathBuf,
    size: u64,
    file_count: u64,
    modified: Option<SystemTime>,
    now: SystemTime,
) -> ScanEntry {
    let age_days = modified.map(|m| age_days_between(m, now));
    ScanEntry {
        rule_id: rule_id.to_string(),
        path,
        size,
        file_count,
        modified,
        age_days,
        recommended: false,
        reason: String::new(),
        selected: false,
    }
}

/// `modified` から `now` までの経過日数（切り捨て）。
///
/// `now` より未来の `modified`（クロックスキュー等）は `0` に丸める。
/// `None` にすると `recommend()` 側で「不明」扱いになり Safe ルールが
/// 推奨 ON になってしまうため、`0`（＝本日更新・使用中の可能性）に倒す方が
/// 安全側（NF-SAF-01）。
fn age_days_between(modified: SystemTime, now: SystemTime) -> u64 {
    now.duration_since(modified)
        .map(|d| d.as_secs() / SECS_PER_DAY)
        .unwrap_or(0)
}

/// `path` の拡張子が `extensions` のいずれかに一致するか（大文字小文字非依存）。
/// 拡張子を持たないパス（ディレクトリ・ドットファイル等）は不一致。
fn matches_extension(path: &Path, extensions: &[String]) -> bool {
    let Some(ext) = path.extension() else {
        return false;
    };
    let ext = ext.to_string_lossy().to_lowercase();
    extensions.iter().any(|e| e.to_lowercase() == ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{KnownDir, PlatformError};
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Write;
    use std::time::Duration;

    // scan_with_progress のコールバックは &mut ローカル変数を掴める。
    // Send 境界が不要であることの回帰テストを兼ねる。
    struct FakePlatform {
        dirs: HashMap<KnownDir, PathBuf>,
    }

    impl FakePlatform {
        fn new() -> Self {
            FakePlatform {
                dirs: HashMap::new(),
            }
        }

        fn with(mut self, kind: KnownDir, path: PathBuf) -> Self {
            self.dirs.insert(kind, path);
            self
        }
    }

    impl Platform for FakePlatform {
        fn known_dir(&self, kind: KnownDir) -> Option<PathBuf> {
            self.dirs.get(&kind).cloned()
        }

        fn to_trash(&self, _path: &Path) -> crate::platform::Result<()> {
            Err(PlatformError::Unsupported("to_trash"))
        }

        fn requires_admin(&self, _path: &Path) -> bool {
            // UnknownPlatform とは異なり、テストでは基点自体を admin 扱いに
            // しない（needs_admin の検証は専用のルールで行う）。
            false
        }

        fn config_dir(&self) -> Option<PathBuf> {
            None
        }

        fn is_elevated(&self) -> bool {
            false
        }

        fn elevate(&self, _args: &[String]) -> crate::platform::ElevateResult {
            Err(crate::platform::ElevateError::Unsupported)
        }
    }

    fn test_rule(
        id: &str,
        base: KnownDir,
        match_kind: MatchKind,
        safety: Safety,
        needs_admin: bool,
        age_threshold_days: Option<u64>,
    ) -> Rule {
        Rule {
            id: id.to_string(),
            label: id.to_string(),
            description: "test".to_string(),
            base,
            match_kind,
            needs_admin,
            safety,
            age_threshold_days,
        }
    }

    fn write_file_with_age(path: &Path, contents: &[u8], age_days: u64) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(path).unwrap();
        file.write_all(contents).unwrap();
        let time = SystemTime::now() - Duration::from_secs(age_days * SECS_PER_DAY + 3600);
        file.set_modified(time).unwrap();
    }

    // ---- (A) 純粋関数 ----

    #[test]
    fn age_days_between_floors_to_whole_days() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(age_days_between(now, now), 0);
        assert_eq!(age_days_between(now - Duration::from_secs(86_399), now), 0);
        assert_eq!(age_days_between(now - Duration::from_secs(86_400), now), 1);
        assert_eq!(
            age_days_between(now - Duration::from_secs(365 * 86_400), now),
            365
        );
        // 未来の mtime は 0 に丸める（安全側）。
        assert_eq!(age_days_between(now + Duration::from_secs(86_400), now), 0);
    }

    #[test]
    fn matches_extension_is_case_insensitive() {
        let exts = vec!["log".to_string()];
        assert!(matches_extension(Path::new("a.LOG"), &exts));
        assert!(matches_extension(Path::new("a.log"), &exts));
        assert!(!matches_extension(Path::new("a.txt"), &exts));
        assert!(!matches_extension(Path::new(".gitignore"), &exts));
        assert!(!matches_extension(Path::new("noext"), &exts));
        assert!(matches_extension(
            Path::new("a.tar.gz"),
            &["gz".to_string()]
        ));
    }

    #[test]
    fn rules_for_safeties_excludes_needs_admin_and_filters_by_safety() {
        let safe_only = rules_for_safeties(&[Safety::Safe]);
        assert!(safe_only.iter().all(|r| r.safety == Safety::Safe));
        assert!(safe_only.iter().all(|r| !r.needs_admin));
        assert!(!safe_only.is_empty());

        let all = rules_for_safeties(&[Safety::Safe, Safety::Caution, Safety::Review]);
        assert!(all.iter().all(|r| !r.needs_admin));
        assert!(all.iter().any(|r| r.id == "old_downloads"));
    }

    #[test]
    fn safety_scope_matches_flow_one_and_flow_two() {
        assert_eq!(safety_scope(false), vec![Safety::Safe]);
        assert_eq!(
            safety_scope(true),
            vec![Safety::Safe, Safety::Caution, Safety::Review]
        );
    }

    const fn assert_send<T: Send>() {}

    #[test]
    fn scan_progress_and_scan_target_are_send() {
        assert_send::<ScanProgress>();
        assert_send::<SkipReason>();
    }

    // ---- (B) 実ファイルシステム上での走査 ----

    #[test]
    fn scan_all_aggregates_top_level_children_and_excludes_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        write_file_with_age(&dir.path().join("top.txt"), b"hello", 10);
        write_file_with_age(&dir.path().join("sub/a.txt"), b"12345", 10);
        write_file_with_age(&dir.path().join("sub/b.txt"), b"1234567890", 5);
        fs::create_dir_all(dir.path().join("empty")).unwrap();

        let platform = FakePlatform::new().with(KnownDir::UserTemp, dir.path().to_path_buf());
        let rule = test_rule(
            "t",
            KnownDir::UserTemp,
            MatchKind::All,
            Safety::Safe,
            false,
            None,
        );

        let entries = scan(&platform, std::slice::from_ref(&rule));

        // "empty" はファイル数0のため候補に含まれない。
        assert_eq!(entries.len(), 2);
        let top = entries
            .iter()
            .find(|e| e.path == dir.path().join("top.txt"))
            .unwrap();
        assert_eq!(top.size, 5);
        assert_eq!(top.file_count, 1);

        let sub = entries
            .iter()
            .find(|e| e.path == dir.path().join("sub"))
            .unwrap();
        assert_eq!(sub.size, 15);
        assert_eq!(sub.file_count, 2);
    }

    #[test]
    fn scan_extension_matches_individual_files_only() {
        let dir = tempfile::tempdir().unwrap();
        write_file_with_age(&dir.path().join("deep/x.log"), b"log", 200);
        write_file_with_age(&dir.path().join("deep/y.txt"), b"txt", 200);

        let platform = FakePlatform::new().with(KnownDir::LocalAppData, dir.path().to_path_buf());
        let rule = test_rule(
            "old_logs",
            KnownDir::LocalAppData,
            MatchKind::Extension(vec!["log".to_string()]),
            Safety::Caution,
            false,
            Some(180),
        );

        let entries = scan(&platform, std::slice::from_ref(&rule));

        assert_eq!(entries.len(), 1, "F-SCAN-08: 合致しないファイルを含めない");
        assert_eq!(entries[0].path, dir.path().join("deep/x.log"));
        assert_eq!(entries[0].file_count, 1);
        assert!(entries[0].recommended, "180日超のログは推奨ON");
    }

    #[test]
    fn scan_older_than_respects_threshold_boundary() {
        let dir = tempfile::tempdir().unwrap();
        write_file_with_age(&dir.path().join("old.bin"), b"x", 90);
        write_file_with_age(&dir.path().join("new.bin"), b"x", 89);

        let platform = FakePlatform::new().with(KnownDir::Downloads, dir.path().to_path_buf());
        let rule = test_rule(
            "old_downloads",
            KnownDir::Downloads,
            MatchKind::OlderThan,
            Safety::Review,
            false,
            Some(90),
        );

        let entries = scan(&platform, std::slice::from_ref(&rule));

        assert_eq!(entries.len(), 1, "しきい値未満は走査結果に含めない");
        assert_eq!(entries[0].path, dir.path().join("old.bin"));
        assert!(!entries[0].recommended, "Review は経過日数によらず既定OFF");
    }

    #[test]
    fn scan_excludes_needs_admin_rules() {
        let dir = tempfile::tempdir().unwrap();
        write_file_with_age(&dir.path().join("a.tmp"), b"x", 10);
        let platform = FakePlatform::new().with(KnownDir::SystemTemp, dir.path().to_path_buf());
        let rule = test_rule(
            "system_temp",
            KnownDir::SystemTemp,
            MatchKind::All,
            Safety::Review,
            true,
            None,
        );

        assert!(scan(&platform, std::slice::from_ref(&rule)).is_empty());
    }

    #[test]
    fn scan_skips_rule_when_known_dir_is_none() {
        let platform = FakePlatform::new(); // どの KnownDir も登録しない
        let rule = test_rule(
            "user_temp",
            KnownDir::UserTemp,
            MatchKind::All,
            Safety::Safe,
            false,
            None,
        );
        assert!(scan(&platform, std::slice::from_ref(&rule)).is_empty());
    }

    #[test]
    fn scan_skips_unreadable_base_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let platform = FakePlatform::new().with(KnownDir::UserTemp, missing);
        let rule = test_rule(
            "user_temp",
            KnownDir::UserTemp,
            MatchKind::All,
            Safety::Safe,
            false,
            None,
        );
        assert!(scan(&platform, std::slice::from_ref(&rule)).is_empty());
    }

    #[test]
    fn scan_older_than_without_threshold_skips_the_rule() {
        let dir = tempfile::tempdir().unwrap();
        write_file_with_age(&dir.path().join("a.bin"), b"x", 9999);
        let platform = FakePlatform::new().with(KnownDir::Downloads, dir.path().to_path_buf());
        let rule = test_rule(
            "old_downloads",
            KnownDir::Downloads,
            MatchKind::OlderThan,
            Safety::Review,
            false,
            None,
        );
        assert!(
            scan(&platform, std::slice::from_ref(&rule)).is_empty(),
            "しきい値未設定の OlderThan ルールは全件拾わず丸ごとスキップする"
        );
    }

    #[test]
    fn all_entries_are_under_the_resolved_base() {
        let dir = tempfile::tempdir().unwrap();
        write_file_with_age(&dir.path().join("a.log"), b"x", 200);
        write_file_with_age(&dir.path().join("sub/b.log"), b"x", 200);
        let platform = FakePlatform::new().with(KnownDir::LocalAppData, dir.path().to_path_buf());
        let rule = test_rule(
            "old_logs",
            KnownDir::LocalAppData,
            MatchKind::Extension(vec!["log".to_string()]),
            Safety::Caution,
            false,
            Some(180),
        );

        let entries = scan(&platform, std::slice::from_ref(&rule));
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert!(entry.path.starts_with(dir.path()), "F-SCAN-08");
        }
    }

    #[test]
    fn scan_with_progress_reports_started_and_finished_and_preserves_rule_order() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        write_file_with_age(&dir_a.path().join("a.txt"), b"x", 10);
        write_file_with_age(&dir_b.path().join("b.txt"), b"x", 10);

        let platform = FakePlatform::new()
            .with(KnownDir::UserTemp, dir_a.path().to_path_buf())
            .with(KnownDir::Downloads, dir_b.path().to_path_buf());
        let rules = vec![
            test_rule(
                "user_temp",
                KnownDir::UserTemp,
                MatchKind::All,
                Safety::Safe,
                false,
                None,
            ),
            test_rule(
                "old_downloads",
                KnownDir::Downloads,
                MatchKind::OlderThan,
                Safety::Review,
                false,
                Some(1),
            ),
        ];

        let mut events = Vec::new();
        let entries = scan_with_progress(&platform, &rules, |event| events.push(event));

        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .iter()
                .map(|e| e.rule_id.as_str())
                .collect::<Vec<_>>(),
            vec!["user_temp", "old_downloads"],
            "9.5: 完了順ではなく入力ルール順であること"
        );

        assert!(matches!(
            events.first(),
            Some(ScanProgress::Started { total_rules: 2 })
        ));
        assert!(matches!(
            events.last(),
            Some(ScanProgress::Finished { entries: 2, .. })
        ));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ScanProgress::RuleFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn scan_reports_skipped_rules_with_reasons() {
        let platform = FakePlatform::new();
        let rules = vec![
            test_rule(
                "system_temp",
                KnownDir::SystemTemp,
                MatchKind::All,
                Safety::Review,
                true,
                None,
            ),
            test_rule(
                "user_temp",
                KnownDir::UserTemp,
                MatchKind::All,
                Safety::Safe,
                false,
                None,
            ),
        ];

        let mut events = Vec::new();
        scan_with_progress(&platform, &rules, |event| events.push(event));

        assert!(events.iter().any(|e| matches!(
            e,
            ScanProgress::RuleSkipped {
                rule_id,
                reason: SkipReason::NeedsAdmin
            } if rule_id == "system_temp"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            ScanProgress::RuleSkipped {
                rule_id,
                reason: SkipReason::UnknownBase
            } if rule_id == "user_temp"
        )));
    }

    #[test]
    fn apply_rule_prefs_respects_always_select_and_exclude() {
        let mut entries = vec![
            ScanEntry {
                rule_id: "a".to_string(),
                path: "/a".into(),
                size: 0,
                file_count: 1,
                modified: None,
                age_days: None,
                recommended: false,
                reason: String::new(),
                selected: false,
            },
            ScanEntry {
                rule_id: "b".to_string(),
                path: "/b".into(),
                size: 0,
                file_count: 1,
                modified: None,
                age_days: None,
                recommended: true,
                reason: String::new(),
                selected: true,
            },
            ScanEntry {
                rule_id: "c".to_string(),
                path: "/c".into(),
                size: 0,
                file_count: 1,
                modified: None,
                age_days: None,
                recommended: true,
                reason: String::new(),
                selected: true,
            },
        ];

        let mut config = Config::default();
        config
            .rule_prefs
            .insert("a".to_string(), RulePref::AlwaysSelect);
        config.rule_prefs.insert("b".to_string(), RulePref::Exclude);
        // "c" は rule_prefs に無い＝ AskEachTime 相当。

        apply_rule_prefs(&mut entries, &config);

        assert!(entries[0].selected, "AlwaysSelect");
        assert!(!entries[1].selected, "Exclude");
        assert!(entries[2].selected, "未設定は recommended のまま");
    }
}
