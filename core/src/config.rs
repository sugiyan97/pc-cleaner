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

/// ユーザー設定。ルール毎の既定、ゴミ箱利用可否、ドライラン既定を保持し永続化される。
///
/// `#[derive(Default)]` は使わず [`Default`] を手書きで実装している。
/// derive すると `use_trash` / `dry_run_default` が `false` になり、
/// F-CFG-02 / F-CFG-03（既定 `true`）および設計目標 G2（ゴミ箱経由・
/// ドライラン既定）に違反するため（本ファイルのテストで固定している）。
///
/// `#[serde(default)]`（コンテナ属性）により、JSON に存在しないフィールドは
/// この `Default` 実装の対応する値で補われる。将来 `age_threshold_days` の
/// `Config` への移設（NF-EXT-03 / C1）等でフィールドを追加しても、既存の
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            rule_prefs: std::collections::HashMap::new(),
            use_trash: true,
            dry_run_default: true,
        }
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
}
