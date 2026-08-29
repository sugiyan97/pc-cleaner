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
use super::{ElevateError, ElevateResult, Platform, PlatformError, Result};
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::iter::once;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE};
#[cfg(windows)]
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
#[cfg(windows)]
use windows::core::PCWSTR;

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
    windows_update_cache: PathBuf,
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
        let windows_update_cache = join_win(&system_root, r"SoftwareDistribution\Download");
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
            windows_update_cache,
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
            KnownDir::WindowsUpdateCache => self.windows_update_cache.clone(),
        }
    }

    /// `path` が管理者権限を要する領域の配下かどうかを、パスの文字列比較で
    /// 判定する。ACL 確認や存在確認（I/O）は行わない初版のヒューリスティックの
    /// ままである。A2（Issue #41）で `is_elevated()` を追加し「今のプロセスが
    /// 昇格しているか」を判定できるようにしたが、これはあくまで別の問い
    /// （このパスは管理者領域か）であり、本メソッドの判定方式自体は変えない。
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

// ---- 権限昇格（A2 / Issue #41）の純粋層 ----
//
// `ShellExecuteExW` の `lpParameters` は「1 本の文字列」であり、受け取った
// 側は `CommandLineToArgvW` 相当のルールで再分解する。空白や `"` を含む
// パス・引数を素で連結すると引数境界が崩れるため、ここで明示的にクォート
// する。`join_win` / `normalize` と同じく、ターゲット依存 API を使わない
// 純粋関数として macOS 上でもテストできる形にする。

/// `CommandLineToArgvW` の規則に従って引数 1 つをクォートする。
///
/// 規則：空文字列は `""`。空白・タブ・`"` を含まなければ素通しする。含む
/// 場合は全体を `"` で囲み、内部の `"` は `\"` に、`"` の直前に来る連続
/// バックスラッシュは 2 倍にする（`C:\foo\` のような末尾 `\` を含む引数を
/// クォートしても、閉じ引用符と結合して壊れないようにするため）。
// 非 Windows ビルドでは呼び出し元（shell_execute_runas 等）が #[cfg(windows)]
// のため未使用扱いになる。from_dirs / is_admin_path と同じ理由で dead_code を
// 許可する（テストからは常に呼ばれ、macOS 上でも検証される）。
#[cfg_attr(not(windows), allow(dead_code))]
fn quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    if !arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return arg.to_string();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                quoted.push('\\');
            }
            '"' => {
                // 直前の連続バックスラッシュを2倍にしてから、クォート自身を
                // エスケープする。
                for _ in 0..backslashes {
                    quoted.push('\\');
                }
                quoted.push('\\');
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                backslashes = 0;
                quoted.push(c);
            }
        }
    }
    // 末尾が連続バックスラッシュのまま閉じクォートに続くと、閉じクォートを
    // エスケープしたと解釈されてしまうため、ここでも2倍にする。
    for _ in 0..backslashes {
        quoted.push('\\');
    }
    quoted.push('"');
    quoted
}

/// 引数列を `lpParameters` 用の1本の文字列へ連結する。
#[cfg_attr(not(windows), allow(dead_code))]
fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `HRESULT` が Win32 エラーコード `code` 由来かを判定する
/// （`HRESULT_FROM_WIN32` の逆算）。
///
/// UAC のキャンセル（`ERROR_CANCELLED` = 1223）を通常の失敗と区別するために
/// 使う。`windows` crate の `WIN32_ERROR` には HRESULT への変換ヘルパーが
/// 無いため、変換規則（`0x8007_0000 | (code & 0xFFFF)`）を自前で持つ。
#[cfg_attr(not(windows), allow(dead_code))]
fn is_win32_error(hresult: i32, code: u32) -> bool {
    (hresult as u32) == (0x8007_0000 | (code & 0xFFFF))
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

/// `windows::core::Error`（`ShellExecuteExW` 失敗時）を、ユーザーが読める
/// 日本語の文へ翻訳する。`windows::core::Error` の中身を `core` の公開 API に
/// 漏らさないため、この結果（`String`）だけを `ElevateError::Failed` に渡す
/// （`trash_error_message` と同じ方針）。
#[cfg(windows)]
fn elevate_error_message(err: &windows::core::Error) -> String {
    format!("{} (code {})", err.message(), err.code().0)
}

/// UTF-16 の NUL 終端バッファへ変換する。`PCWSTR` はこのバッファへの
/// 借用ポインタであり、呼び出し元は `ShellExecuteExW` を呼び終えるまで
/// バッファを生かしておく必要がある。
#[cfg(windows)]
fn to_wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(once(0)).collect()
}

/// プロセストークンの `TokenElevation` を読み、昇格済みかを返す。
///
/// `IsUserAnAdmin()` は Microsoft が非推奨としており、また「トークンが
/// Administrators グループを含むか」しか見ないため、UAC で分割された
/// 非昇格トークンと昇格トークンを取り違えうる（Built-in Administrator や
/// UAC 無効環境等）。取り違えは「昇格したのに `is_elevated()` が `false` の
/// まま無限に再昇格する」事故に直結するため、問いたい内容（トークンが
/// 昇格しているか）をそのまま問う `TokenElevation` を使う。
/// いずれかの手順が失敗した場合は安全側（`false`）に倒す（NF-SAF-01）。
// WinAPI 呼び出しのため、crate 全体の #![deny(unsafe_code)] をこの関数
// だけで局所的に解除する（crate 属性自体は絶対に外さないこと）。
#[cfg(windows)]
#[allow(unsafe_code)]
fn query_is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        let _ = CloseHandle(token);

        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// 自プロセスを `ShellExecuteExW`（`lpVerb = "runas"`）で管理者権限として
/// 起動し直す。呼び出し元（CLI/GUI）はこの成功後に自プロセスを終了させる
/// 責務を持つ（`elevate` の doc コメント参照）。
// WinAPI 呼び出しのため、この関数だけ #![deny(unsafe_code)] を局所解除する。
#[cfg(windows)]
#[allow(unsafe_code)]
fn shell_execute_runas(exe: &Path, args: &[String]) -> ElevateResult {
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();

    let file = to_wide(exe.as_os_str());
    let params = to_wide(std::ffi::OsStr::new(&join_args(args)));
    let dir = to_wide(exe_dir.as_os_str());
    let verb = to_wide(std::ffi::OsStr::new("runas"));

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // SEE_MASK_NOASYNC: ShellExecuteEx が内部処理を完了してから戻る
        //   ことを保証する。呼び出し直後に自プロセスを終了させるため必要。
        // SEE_MASK_FLAG_NO_UI: OS 既定のエラーダイアログを抑止し、失敗は
        //   自前の日本語メッセージで伝える（#35 と同じ方針）。UAC の同意
        //   画面自体はこのフラグでは抑止されない。
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: 1, // SW_SHOWNORMAL
        ..Default::default()
    };

    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(()),
        // ユーザーが UAC で「いいえ」を選ぶと ERROR_CANCELLED(1223) が
        // HRESULT として返る。通常の失敗と区別する（F-ELV-04）。
        Err(e) if is_win32_error(e.code().0, ERROR_CANCELLED.0) => Err(ElevateError::Cancelled),
        Err(e) => Err(ElevateError::Failed(elevate_error_message(&e))),
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

    fn is_elevated(&self) -> bool {
        query_is_elevated()
    }

    fn elevate(&self, args: &[String]) -> ElevateResult {
        // 二重防御の1つ目：既に昇格済みなら WinAPI を一切呼ばずに返す。これに
        // より、管理者権限で動く CI ランナー（windows-latest）上で誤って
        // 呼ばれても実際にプロセスが生えることはない。
        if self.is_elevated() {
            return Err(ElevateError::AlreadyElevated);
        }

        let exe = std::env::current_exe()
            .map_err(|e| ElevateError::CurrentExeUnavailable(e.to_string()))?;
        shell_execute_runas(&exe, args)
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
    fn known_dir_resolves_all_eight_kinds() {
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
        assert_eq!(
            platform.resolve(KnownDir::WindowsUpdateCache),
            PathBuf::from(r"C:\Windows\SoftwareDistribution\Download")
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

    /// A1（Issue #40）の昇格ゲーティングが A3（Issue #42）の新ルールにも
    /// そのまま効くための前提条件を固定する回帰テスト。`windows_update_cache`
    /// の基点は `system_root`（`admin_roots` に含まれる）の配下に合成して
    /// いるため、`admin_roots` / `is_admin_path` を変更しなくても管理者
    /// 領域として判定されるはずである。ここが崩れると、A1 の「昇格済みかつ
    /// `needs_admin` なルールのみ許可」というゲートが実質的に効かなくなる。
    #[test]
    fn requires_admin_detects_windows_update_cache() {
        let platform = fixture();
        assert!(platform.is_admin_path(Path::new(
            r"C:\Windows\SoftwareDistribution\Download\update.cab"
        )));
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

    // ---- 権限昇格（A2 / Issue #41）の純粋層 ----

    #[test]
    fn quote_arg_passes_through_simple_values() {
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("scan"), "scan");
        assert_eq!(quote_arg("--admin"), "--admin");
        assert_eq!(quote_arg(r"C:\foo\bar.exe"), r"C:\foo\bar.exe");
    }

    #[test]
    fn quote_arg_wraps_values_with_whitespace() {
        assert_eq!(quote_arg("hello world"), "\"hello world\"");
        assert_eq!(quote_arg("a\tb"), "\"a\tb\"");
    }

    #[test]
    fn quote_arg_escapes_embedded_quotes() {
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
    }

    #[test]
    fn quote_arg_doubles_backslashes_before_closing_quote() {
        // 末尾が連続バックスラッシュのまま閉じクォートに続くと、閉じクォート
        // をエスケープしたと誤読される（CommandLineToArgvW の規則）ため、
        // 閉じクォート直前のバックスラッシュは2倍にする必要がある。
        // 手動でのエスケープ記述ミスを避けるため、期待値は push で組み立てる。
        let input = r"C:\Program Files\"; // 空白を含むため引用され、末尾が \ の実例。
        let mut expected = String::new();
        expected.push('"');
        expected.push_str(r"C:\Program Files");
        expected.push('\\');
        expected.push('\\');
        expected.push('"');
        assert_eq!(quote_arg(input), expected);
    }

    #[test]
    fn join_args_joins_with_single_space() {
        assert_eq!(join_args(&[]), "");
        assert_eq!(join_args(&["scan".to_string()]), "scan");
        assert_eq!(
            join_args(&["clean".to_string(), "--admin".to_string()]),
            "clean --admin"
        );
        assert_eq!(
            join_args(&["a b".to_string(), "c".to_string()]),
            "\"a b\" c"
        );
    }

    #[test]
    fn is_win32_error_matches_hresult_from_win32() {
        // ERROR_CANCELLED (1223 = 0x4C7) -> 0x800704C7。
        assert!(is_win32_error(0x800704C7u32 as i32, 1223));
        assert!(!is_win32_error(0x80070005u32 as i32, 1223));
    }
}

#[cfg(windows)]
#[cfg(test)]
mod elevate_tests {
    use super::*;

    // ⚠️ windows-latest の GitHub Actions ランナーは管理者権限で動作して
    // いることが多く、`is_elevated()` の実際の値は実行環境に依存する
    // （開発者のデスクトップでは false、CI では true になりうる）。
    // 値そのものを assert すると必ずどちらかの環境で壊れるため、ここでは
    // パニックしないことだけを確認する。`elevate()` 自体は実プロセスを
    // 起動しうるため、実 OS バインディングを呼ぶテストはここでも書かない。
    #[test]
    fn is_elevated_does_not_panic() {
        let _ = WindowsPlatform::new().is_elevated();
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
