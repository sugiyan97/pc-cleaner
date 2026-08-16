//! ユーザー設定の保存・読込（F-CFG-01〜05）。永続化そのものは #8 で実装する。

use std::collections::HashMap;

/// ルール毎のユーザー既定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// ルール毎の既定（常に選択 / 除外 / 毎回確認）。
    pub rule_prefs: HashMap<String, RulePref>,
    /// ゴミ箱経由での削除を既定とするか。
    pub use_trash: bool,
    /// ドライランを既定動作とするか。
    pub dry_run_default: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            rule_prefs: HashMap::new(),
            use_trash: true,
            dry_run_default: true,
        }
    }
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
}
