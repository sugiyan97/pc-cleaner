//! Windows 向け [`Platform`](super::Platform) 実装。
//!
//! `#[cfg(windows)]` は本ファイルの中にだけ登場させる（要件 4.2 / NF-MNT-03）。
//! パス解決とゴミ箱送りのみをここに置き、ルールとロジックは OS 非依存のまま
//! 保つ（core 内の他モジュールは `Platform` trait 越しにのみここへ触れる）。
//!
//! 実装は「値の保持と合成ロジック（cfg なし、macOS 上でもコンパイル・テスト
//! される純粋層）」と「`std::env::var` を読む OS バインディング層
//! （`#[cfg(windows)]`）」に分割している。合成・比較は `std::path::Path` の
//! `join` / `components` 等の**ターゲット依存 API を使わず**、`\` 区切りを
//! 明示的に扱う文字列ベースで行う。これにより macOS 上のテストがそのまま
//! Windows 上の挙動を保証する。

use super::KnownDir;
#[cfg(windows)]
use super::{Platform, PlatformError, Result};
use std::path::{Path, PathBuf};

/// Windows の既知フォルダ・管理者権限領域の基点。
///
/// 値の取得（環境変数読み取り）と、値からの合成・判定ロジックを分離するため
/// 独立した構造体にしている。`known_dir` は存在確認（I/O）を行わない。
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) struct WindowsPlatform {
    user_temp: PathBuf,
    system_temp: PathBuf,
    local_app_data: PathBuf,
    cache: PathBuf,
    recycle_bin: PathBuf,
    downloads: PathBuf,
    thumbnail_cache: PathBuf,
    /// アプリ設定の保存先（`%APPDATA%\pc-cleaner`）。
    config_dir: PathBuf,
    /// 管理者権限を要すると判定する基点の一覧。
    admin_roots: Vec<PathBuf>,
}

// ---- 純粋層（cfg なし。macOS 上でもコンパイル・テストされる）----
#[cfg_attr(not(windows), allow(dead_code))]
impl WindowsPlatform {
    /// 6 つの基点（`%TEMP%` / `%LOCALAPPDATA%` / `%USERPROFILE%` /
    /// `%SystemRoot%` / `%SystemDrive%` / `%APPDATA%`）から全フィールドを合成する。
    fn from_dirs(
        user_temp: PathBuf,
        local_app_data: PathBuf,
        user_profile: PathBuf,
        system_root: PathBuf,
        system_drive: PathBuf,
        app_data: PathBuf,
    ) -> Self {
        let system_temp = join_win(&system_root, "Temp");
        let cache = join_win(&local_app_data, r"Microsoft\Windows\INetCache");
        let recycle_bin = join_win(&system_drive, r"$Recycle.Bin");
        let downloads = join_win(&user_profile, "Downloads");
        let thumbnail_cache = join_win(&local_app_data, r"Microsoft\Windows\Explorer");
        let config_dir = join_win(&app_data, "pc-cleaner");

        // Program Files の実パスは %ProgramFiles% 等で上書きされうるが、初版の
        // ヒューリスティックでは %SystemDrive% からの既定位置で近似する。
        let admin_roots = vec![
            system_root.clone(),
            join_win(&system_drive, "Program Files"),
            join_win(&system_drive, "Program Files (x86)"),
            join_win(&system_drive, "ProgramData"),
        ];

        WindowsPlatform {
            user_temp,
            system_temp,
            local_app_data,
            cache,
            recycle_bin,
            downloads,
            thumbnail_cache,
            config_dir,
            admin_roots,
        }
    }

    /// `kind` に対応する既知フォルダを返す。存在確認は行わない。
    fn resolve(&self, kind: KnownDir) -> PathBuf {
        match kind {
            KnownDir::UserTemp => self.user_temp.clone(),
            KnownDir::SystemTemp => self.system_temp.clone(),
            KnownDir::LocalAppData => self.local_app_data.clone(),
            KnownDir::Cache => self.cache.clone(),
            KnownDir::RecycleBin => self.recycle_bin.clone(),
            KnownDir::Downloads => self.downloads.clone(),
            KnownDir::ThumbnailCache => self.thumbnail_cache.clone(),
        }
    }

    /// `path` が管理者権限を要する領域の配下かどうかを、パスの文字列比較で
    /// 判定する。ACL 確認や存在確認（I/O）は行わない初版のヒューリスティック
    /// であり、真の権限判定は将来対応 A2 で行う。
    fn is_admin_path(&self, path: &Path) -> bool {
        let target = normalize(path);
        self.admin_roots
            .iter()
            .any(|root| is_under(&target, &normalize(root)))
    }
}

/// Windows の区切り文字（`\`）で明示的に連結する。
///
/// `std::path::Path::join` はターゲット依存（macOS では `/` で連結される）
/// のため使わない。
fn join_win(base: &Path, rest: &str) -> PathBuf {
    let base = base.to_string_lossy();
    let base = base.trim_end_matches('\\');
    PathBuf::from(format!("{base}\\{rest}"))
}

/// 比較用に正規化する：`/` を `\` に統一し、末尾の `\` を除去し、小文字化する。
fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// `path` が `base` 直下または配下かどうかを判定する。
///
/// 単純な `str::starts_with` だと `C:\WindowsApps` が `C:\Windows` に
/// 誤マッチするため、区切り文字での境界を明示的に見る。
fn is_under(path: &str, base: &str) -> bool {
    path == base || path.starts_with(&format!("{base}\\"))
}

// ---- OS バインディング層（Windows のみ）----

/// `trash::delete` が返す `trash::Error` を、ユーザーが読める日本語の文へ翻訳する。
///
/// `trash::Error` の `Display` は常に Debug ダンプ（`Error during a
/// \`trash\` operation: {self:?}`）を出すため、GUI/CLI にそのまま出すと
/// 判読不能になる（#35）。ここでバリアントごとに自然文へ変換し、
/// `PlatformError::Trash(String)` にはこの結果だけを渡す。
#[cfg(windows)]
fn trash_error_message(err: &trash::Error) -> String {
    match err {
        trash::Error::TargetedRoot => {
            "ドライブ直下やルートフォルダは安全のためゴミ箱に送れません。".to_string()
        }
        trash::Error::CouldNotAccess { target } => format!(
            "{target} にアクセスできませんでした。別のアプリで使用中か、アクセス権限が不足している可能性があります。"
        ),
        trash::Error::Os { code, description } => format!(
            "OS がゴミ箱操作を拒否しました（コード {code}: {description}）。他のアプリで使用中でないか、アクセス権限を確認してください。"
        ),
        trash::Error::CanonicalizePath { original } => format!(
            "{} のパスを解決できませんでした。ファイルが移動または削除された可能性があります。",
            original.display()
        ),
        trash::Error::ConvertOsString { .. } => {
            "ファイル名に扱えない文字が含まれているため処理できませんでした。".to_string()
        }
        trash::Error::Unknown { description } => format!(
            "ゴミ箱への移動に失敗しました（{description}）。別のアプリがファイルを使用しているか、アクセス権限がない可能性があります。しばらく待つか、該当のアプリを閉じてから再試行してください。"
        ),
        // RestoreCollision / RestoreTwins は復元専用でゴミ箱送りでは発生しない。
        // 将来クレートが分岐を増やしても壊れないよう汎用文にフォールバックする。
        _ => "ゴミ箱への移動中に予期しないエラーが発生しました。".to_string(),
    }
}

#[cfg(windows)]
impl WindowsPlatform {
    pub(super) fn new() -> Self {
        use std::env;

        let user_profile = env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(r"C:\Users\Default"));
        let local_app_data = env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| join_win(&user_profile, r"AppData\Local"));
        let user_temp = env::var("TEMP")
            .or_else(|_| env::var("TMP"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| join_win(&local_app_data, "Temp"));
        let system_root = env::var("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(r"C:\Windows"));
        let system_drive = env::var("SystemDrive")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:"));
        let app_data = env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| join_win(&user_profile, r"AppData\Roaming"));

        WindowsPlatform::from_dirs(
            user_temp,
            local_app_data,
            user_profile,
            system_root,
            system_drive,
            app_data,
        )
    }
}

#[cfg(windows)]
impl Platform for WindowsPlatform {
    fn known_dir(&self, kind: KnownDir) -> Option<PathBuf> {
        Some(self.resolve(kind))
    }

    fn to_trash(&self, path: &Path) -> Result<()> {
        trash::delete(path).map_err(|e| PlatformError::Trash(trash_error_message(&e)))
    }

    fn requires_admin(&self, path: &Path) -> bool {
        self.is_admin_path(path)
    }

    fn config_dir(&self) -> Option<PathBuf> {
        Some(self.config_dir.clone())
    }
}

#[cfg(windows)]
pub(super) use self::WindowsPlatform as PlatformImpl;

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> WindowsPlatform {
        WindowsPlatform::from_dirs(
            PathBuf::from(r"C:\Users\alice\AppData\Local\Temp"),
            PathBuf::from(r"C:\Users\alice\AppData\Local"),
            PathBuf::from(r"C:\Users\alice"),
            PathBuf::from(r"C:\Windows"),
            PathBuf::from(r"C:"),
            PathBuf::from(r"C:\Users\alice\AppData\Roaming"),
        )
    }

    #[test]
    fn config_dir_resolves_under_app_data_roaming() {
        let platform = fixture();
        assert_eq!(
            platform.config_dir,
            PathBuf::from(r"C:\Users\alice\AppData\Roaming\pc-cleaner")
        );
    }

    #[test]
    fn known_dir_resolves_all_seven_kinds() {
        let platform = fixture();
        assert_eq!(
            platform.resolve(KnownDir::UserTemp),
            PathBuf::from(r"C:\Users\alice\AppData\Local\Temp")
        );
        assert_eq!(
            platform.resolve(KnownDir::SystemTemp),
            PathBuf::from(r"C:\Windows\Temp")
        );
        assert_eq!(
            platform.resolve(KnownDir::LocalAppData),
            PathBuf::from(r"C:\Users\alice\AppData\Local")
        );
        assert_eq!(
            platform.resolve(KnownDir::Cache),
            PathBuf::from(r"C:\Users\alice\AppData\Local\Microsoft\Windows\INetCache")
        );
        assert_eq!(
            platform.resolve(KnownDir::RecycleBin),
            PathBuf::from(r"C:\$Recycle.Bin")
        );
        assert_eq!(
            platform.resolve(KnownDir::Downloads),
            PathBuf::from(r"C:\Users\alice\Downloads")
        );
        assert_eq!(
            platform.resolve(KnownDir::ThumbnailCache),
            PathBuf::from(r"C:\Users\alice\AppData\Local\Microsoft\Windows\Explorer")
        );
    }

    #[test]
    fn requires_admin_detects_system_directories() {
        let platform = fixture();
        assert!(platform.is_admin_path(Path::new(r"C:\Windows\Temp\foo.tmp")));
        assert!(platform.is_admin_path(Path::new(r"C:\Program Files\App\bin.exe")));
        assert!(platform.is_admin_path(Path::new(r"C:\ProgramData\App\config.ini")));
        assert!(!platform.is_admin_path(Path::new(r"C:\Users\alice\AppData\Local\Temp\foo.tmp")));
    }

    #[test]
    fn requires_admin_does_not_match_sibling_prefix() {
        let platform = fixture();
        // "C:\WindowsApps" は "C:\Windows" の兄弟ディレクトリであり配下ではない。
        assert!(!platform.is_admin_path(Path::new(r"C:\WindowsApps\Foo\bin.exe")));
    }

    #[test]
    fn requires_admin_is_case_insensitive() {
        let platform = fixture();
        assert!(platform.is_admin_path(Path::new(r"c:\windows\temp\foo.tmp")));
    }

    #[test]
    fn requires_admin_matches_base_directory_itself() {
        let platform = fixture();
        assert!(platform.is_admin_path(Path::new(r"C:\Windows")));
    }
}

#[cfg(windows)]
#[cfg(test)]
mod trash_error_tests {
    use super::trash_error_message;

    #[test]
    fn unknown_variant_produces_actionable_japanese_text() {
        let err = trash::Error::Unknown {
            description: "Some operations were aborted".to_string(),
        };
        let msg = trash_error_message(&err);
        assert!(msg.contains("別のアプリ") || msg.contains("アクセス権限"));
        assert!(!msg.contains("Error during a `trash` operation"));
        assert!(!msg.contains("Unknown {"));
    }

    #[test]
    fn os_variant_includes_error_code() {
        let err = trash::Error::Os {
            code: 5,
            description: "Access is denied.".to_string(),
        };
        let msg = trash_error_message(&err);
        assert!(msg.contains('5'));
    }

    #[test]
    fn targeted_root_explains_refusal() {
        let msg = trash_error_message(&trash::Error::TargetedRoot);
        assert!(!msg.is_empty());
        assert!(!msg.contains("TargetedRoot"));
    }

    #[test]
    fn could_not_access_mentions_target() {
        let err = trash::Error::CouldNotAccess {
            target: r"C:\foo\bar.tmp".to_string(),
        };
        let msg = trash_error_message(&err);
        assert!(msg.contains(r"C:\foo\bar.tmp"));
    }
}
