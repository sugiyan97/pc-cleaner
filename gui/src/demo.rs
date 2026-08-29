//! 開発用のサンドボックス `Platform`（`demo` feature 時のみコンパイルされる）。
//!
//! macOS 等の非 Windows 環境では `platform::current()` が `UnknownPlatform`
//! になり、`known_dir` が常に `None` を返すため走査結果が常に空になる。
//! `scan()` / `preview()` / `execute()` の実コードパスを目視確認するために
//! 一時ディレクトリ配下だけで完結するサンプルデータを用意する。
//! ユーザーの実ファイルには一切触れない。

use pc_cleaner_core::platform::{
    ElevateError, ElevateResult, KnownDir, Platform, PlatformError, Result,
};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// サンプルデータを一時ディレクトリに用意して振る舞う `Platform`。
pub struct DemoPlatform {
    root: PathBuf,
}

impl DemoPlatform {
    /// `%TEMP%/pc-cleaner-demo` 相当のサンドボックスを用意する。
    pub fn new() -> Self {
        let root = std::env::temp_dir().join("pc-cleaner-demo");
        let platform = DemoPlatform { root };
        platform.ensure_seeded();
        platform
    }

    fn ensure_seeded(&self) {
        if self.root.join("user_temp").exists() {
            return;
        }
        self.seed(
            &self.root.join("user_temp"),
            &[("today.tmp", 0), ("old.tmp", 30)],
        );
        self.seed(&self.root.join("cache"), &[("cache.dat", 5)]);
        self.seed(
            &self.root.join("thumbnail_cache"),
            &[("thumbcache_001.db", 2)],
        );
        self.seed(
            &self.root.join("downloads"),
            &[("installer.exe", 120), ("report.pdf", 10)],
        );
        self.seed(
            &self.root.join("logs"),
            &[("app.log", 200), ("recent.log", 3)],
        );
        self.seed(
            &self.root.join("recycle_bin").join("SAMPLE-SID"),
            &[("deleted.txt", 1)],
        );
        let _ = fs::create_dir_all(self.root.join("trash"));
        let _ = fs::create_dir_all(self.root.join("config"));
    }

    fn seed(&self, dir: &Path, files: &[(&str, u64)]) {
        let _ = fs::create_dir_all(dir);
        for (name, age_days) in files {
            let path = dir.join(name);
            if File::create(&path).is_err() {
                continue;
            }
            if let Ok(file) = OpenOptions::new().write(true).open(&path) {
                let age = Duration::from_secs(age_days * 24 * 60 * 60 + 3600);
                if let Some(time) = SystemTime::now().checked_sub(age) {
                    let _ = file.set_modified(time);
                }
            }
        }
    }
}

impl Platform for DemoPlatform {
    fn known_dir(&self, kind: KnownDir) -> Option<PathBuf> {
        let sub = match kind {
            KnownDir::UserTemp => "user_temp",
            KnownDir::SystemTemp => return None, // needs_admin のため実際には呼ばれない
            KnownDir::LocalAppData => "logs",
            KnownDir::Cache => "cache",
            KnownDir::RecycleBin => "recycle_bin",
            KnownDir::Downloads => "downloads",
            KnownDir::ThumbnailCache => "thumbnail_cache",
        };
        Some(self.root.join(sub))
    }

    fn to_trash(&self, path: &Path) -> Result<()> {
        let file_name = path
            .file_name()
            .ok_or_else(|| PlatformError::Trash("invalid path".to_string()))?;
        let dest = self.root.join("trash").join(file_name);
        fs::rename(path, dest).map_err(PlatformError::from)
    }

    fn requires_admin(&self, _path: &Path) -> bool {
        false
    }

    fn config_dir(&self) -> Option<PathBuf> {
        Some(self.root.join("config"))
    }

    fn is_elevated(&self) -> bool {
        false
    }

    fn elevate(&self, _args: &[String]) -> ElevateResult {
        Err(ElevateError::Unsupported)
    }
}
