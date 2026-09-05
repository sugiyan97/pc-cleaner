//! ユーザー設定の保存・読込（F-CFG-01〜05）。

use crate::platform::Platform;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// ルール毎のユーザー既定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RulePref {
    /// 常に選択する。
    AlwaysSelect,
    /// 常に除外する。
    Exclude,
    /// 毎回確認する（`recommend()` の判定に従う、最も安全な既定）。
    #[default]
    AskEachTime,
}

/// ユーザー設定。ルール毎の既定、ゴミ箱利用可否、ドライラン既定、経過日数
/// しきい値の上書きを保持し永続化される。
///
/// `#[derive(Default)]` は使わず [`Default`] を手書きで実装している。
/// derive すると `use_trash` / `dry_run_default` が `false` になり、
/// F-CFG-02 / F-CFG-03（既定 `true`）および設計目標 G2（ゴミ箱経由・
/// ドライラン既定）に違反するため（本ファイルのテストで固定している）。
///
/// `#[serde(default)]`（コンテナ属性）により、JSON に存在しないフィールドは
/// この `Default` 実装の対応する値で補われる。これにより `age_thresholds`
/// （NF-EXT-03 / C1 で追加）のようなフィールドを追加しても、既存の
/// `config.json` の読み込みが壊れない（F-CFG-05 の趣旨）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// ルール毎の既定（常に選択 / 除外 / 毎回確認）。
    pub rule_prefs: std::collections::HashMap<String, RulePref>,
    /// ゴミ箱経由での削除を既定とするか。
    pub use_trash: bool,
    /// ドライランを既定動作とするか。
    pub dry_run_default: bool,
    /// ルール `id` ごとの経過日数しきい値の上書き（日）。未設定のルールは
    /// `Rule` 定義側の既定値を使う（NF-EXT-03 / C1 / Issue #47）。
    pub age_thresholds: std::collections::HashMap<String, u64>,
    /// 「サイズが大きい」と見なすしきい値（バイト単位）。C2 / Issue #48。
    ///
    /// `Rule::large_file_threshold_bytes` の元になる値。あくまで
    /// `recommend()` が付け加える注意書き（`reason` への追記）のためのもので、
    /// 推奨可否そのものには影響しない。
    pub large_file_threshold_bytes: u64,
    /// 走査時に重複ファイルを検出するか（C2 / Issue #48）。
    ///
    /// 内容ハッシュの計算を伴い走査コストが増えるため既定は `false`。
    pub detect_duplicates: bool,
}

/// 既定の大容量ファイルしきい値（1 GiB）。
const DEFAULT_LARGE_FILE_THRESHOLD_BYTES: u64 = 1_073_741_824;

impl Default for Config {
    fn default() -> Self {
        Config {
            rule_prefs: std::collections::HashMap::new(),
            use_trash: true,
            dry_run_default: true,
            age_thresholds: std::collections::HashMap::new(),
            large_file_threshold_bytes: DEFAULT_LARGE_FILE_THRESHOLD_BYTES,
            detect_duplicates: false,
        }
    }
}

/// `age_thresholds` に設定できる経過日数の下限（日）。
pub const AGE_THRESHOLD_MIN_DAYS: u64 = 1;
/// `age_thresholds` に設定できる経過日数の上限（日）。
pub const AGE_THRESHOLD_MAX_DAYS: u64 = 3650;

impl Config {
    /// `rule_id` の経過日数しきい値を返す。`age_thresholds` に設定が無ければ
    /// `default_days` を使う。手編集等で壊れた値が危険なしきい値（0 日や
    /// 極端に大きい日数）にならないよう、結果は必ず
    /// `[AGE_THRESHOLD_MIN_DAYS, AGE_THRESHOLD_MAX_DAYS]` の範囲に丸める
    /// （NF-EXT-03 / C1 / Issue #47）。
    pub fn age_threshold_days(&self, rule_id: &str, default_days: u64) -> u64 {
        let value = self
            .age_thresholds
            .get(rule_id)
            .copied()
            .unwrap_or(default_days);
        value.clamp(AGE_THRESHOLD_MIN_DAYS, AGE_THRESHOLD_MAX_DAYS)
    }
}

/// 設定ファイル名。
const CONFIG_FILE_NAME: &str = "config.json";

/// `Platform::config_dir()` を用いて設定ファイルのフルパス
/// （`<config_dir>/config.json`）を解決する。`config_dir` が解決できない
/// 場合は `None`。
///
/// 実際のファイル I/O は行わない。生パスを `core` の他ロジックに持ち込まず
/// `Platform` 越しにのみ解決する（要件 4.2 / NF-MNT-03）。
pub fn config_file_path(platform: &dyn Platform) -> Option<PathBuf> {
    platform.config_dir().map(|dir| dir.join(CONFIG_FILE_NAME))
}

/// 設定ファイルを読み込む。ファイルが存在しない、または内容が壊れている
/// 場合は既定値へフォールバックし、起動を妨げない（F-CFG-05）。
pub fn load(path: &Path) -> Config {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// 設定を `path` に保存する。親ディレクトリが存在しなければ作成する。
pub fn save(config: &Config, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_trash_and_dry_run() {
        let config = Config::default();
        assert!(config.use_trash, "F-CFG-02: use_trash の既定値は true");
        assert!(
            config.dry_run_default,
            "F-CFG-03: dry_run_default の既定値は true"
        );
        assert!(config.rule_prefs.is_empty());
        assert!(config.age_thresholds.is_empty());
        assert_eq!(
            config.large_file_threshold_bytes, 1_073_741_824,
            "既定値は1 GiB"
        );
        assert!(!config.detect_duplicates, "走査コストのため既定は false");
    }

    #[test]
    fn default_rule_pref_is_ask_each_time() {
        assert_eq!(RulePref::default(), RulePref::AskEachTime);
    }

    #[test]
    fn load_returns_default_when_file_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert_eq!(load(&path), Config::default());
    }

    #[test]
    fn load_returns_default_when_file_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "not valid json").unwrap();
        assert_eq!(load(&path), Config::default());
    }

    #[test]
    fn load_returns_default_for_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "{}").unwrap();
        assert_eq!(load(&path), Config::default());
    }

    #[test]
    fn save_then_load_round_trips_rule_prefs_and_use_trash() {
        let dir = tempfile::tempdir().unwrap();
        // 親ディレクトリが存在しない場合の作成も合わせて確認する。
        let path = dir.path().join("nested").join("config.json");

        let mut rule_prefs = std::collections::HashMap::new();
        rule_prefs.insert("old_logs".to_string(), RulePref::AlwaysSelect);
        rule_prefs.insert("old_downloads".to_string(), RulePref::Exclude);
        let config = Config {
            rule_prefs,
            use_trash: false,
            ..Config::default()
        };

        save(&config, &path).unwrap();
        assert_eq!(load(&path), config);
    }

    #[test]
    fn load_accepts_config_written_before_age_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        // age_thresholds が存在しなかった頃の config.json を模す。
        fs::write(
            &path,
            r#"{"rule_prefs":{},"use_trash":true,"dry_run_default":true}"#,
        )
        .unwrap();

        let config = load(&path);
        assert!(config.age_thresholds.is_empty());
        assert!(config.use_trash);
        assert!(config.dry_run_default);
    }

    #[test]
    fn save_then_load_round_trips_age_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        let mut age_thresholds = std::collections::HashMap::new();
        age_thresholds.insert("old_logs".to_string(), 30);
        age_thresholds.insert("old_downloads".to_string(), 14);
        let config = Config {
            age_thresholds,
            ..Config::default()
        };

        save(&config, &path).unwrap();
        assert_eq!(load(&path), config);
    }

    #[test]
    fn age_threshold_days_falls_back_to_default_when_unset() {
        let config = Config::default();
        assert_eq!(config.age_threshold_days("old_logs", 180), 180);
    }

    #[test]
    fn age_threshold_days_uses_configured_override() {
        let mut config = Config::default();
        config.age_thresholds.insert("old_logs".to_string(), 30);
        assert_eq!(config.age_threshold_days("old_logs", 180), 30);
    }

    #[test]
    fn age_threshold_days_clamps_to_the_safe_range() {
        let mut config = Config::default();
        config.age_thresholds.insert("too_low".to_string(), 0);
        config.age_thresholds.insert("too_high".to_string(), 99999);

        assert_eq!(
            config.age_threshold_days("too_low", 180),
            AGE_THRESHOLD_MIN_DAYS
        );
        assert_eq!(
            config.age_threshold_days("too_high", 180),
            AGE_THRESHOLD_MAX_DAYS
        );
    }
}
