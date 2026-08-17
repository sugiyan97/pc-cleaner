//! OS 固有の知識を隠蔽する抽象層。
//!
//! `KnownDir` と `Platform` trait は本モジュールで定義する。OS ごとの実装は
//! `windows.rs` / `unknown.rs` に置き、このファイルからは一切 `#[cfg]` や
//! OS 固有 API を参照しない（要件 4.2 / NF-MNT-03）。
//!
//! どちらの実装を使うかは、各 OS ファイルが自分の中の `#[cfg]` で
//! `PlatformImpl` という同じ名前を選択的に公開することで決まる。本ファイルは
//! 両方を glob import するだけで、OS 条件そのものは登場しない。将来の OS 対応
//! （NF-EXT-02）は `platform/` にファイルを追加し、下に2行足すだけでよい。
//! （新 OS を追加する場合、`unknown.rs` 側の `#[cfg(not(windows))]` を
//! `#[cfg(not(any(windows, target_os = "..."))))]` のように狭める必要がある）

mod unknown;
mod windows;

use std::fmt;
use std::path::{Path, PathBuf};

#[allow(unused_imports)]
use self::unknown::*;
#[allow(unused_imports)]
use self::windows::*;

/// 実行中の OS に対応する [`Platform`] 実装を生成する。
pub fn current() -> Box<dyn Platform> {
    Box::new(PlatformImpl::new())
}

/// OS 固有の実パスを直接書かずに走査基点を指定するための抽象キー。
///
/// ルール定義（[`crate::rule::Rule`]）にはここに列挙されたキーのみを用い、
/// 生のパス文字列を書いてはならない（要件 4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KnownDir {
    /// ユーザー一時ファイル領域（Windows では `%TEMP%`）。
    UserTemp,
    /// システム一時ファイル領域（Windows では `C:\Windows\Temp`）。
    /// 将来対応 A1 まで `needs_admin = true` としてフィルタ除外される。
    SystemTemp,
    /// ユーザーごとのアプリケーションデータ領域（Windows では `%LOCALAPPDATA%`）。
    LocalAppData,
    /// 各種キャッシュの基点。
    Cache,
    /// ゴミ箱。
    RecycleBin,
    /// ダウンロードフォルダ（Windows では `%USERPROFILE%\Downloads`）。
    Downloads,
    /// サムネイルキャッシュ（Windows では
    /// `%LOCALAPPDATA%\Microsoft\Windows\Explorer`）。
    ///
    /// `Cache`（`INetCache`）とは別領域。`LocalAppData` 全体を基点にすると
    /// 無関係な `.db` ファイルまで巻き込むため、専用の基点として分離している
    /// （Issue #16）。
    ThumbnailCache,
}

/// [`Platform`] の操作が失敗した際のエラー。
///
/// ゴミ箱送りの実装詳細（`trash` クレート等）は `platform/windows.rs` の中に
/// 閉じ込め、そのエラー型を `core` の公開 API に漏らさないよう文字列化する。
#[derive(Debug)]
pub enum PlatformError {
    /// ファイルシステム操作に伴う I/O エラー。
    Io(std::io::Error),
    /// ゴミ箱送りの実装（`trash` クレート等）が返したエラーメッセージ。
    Trash(String),
    /// この OS ではサポートされない操作（`unknown.rs` スタブ用、NF-OS-02）。
    Unsupported(&'static str),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::Io(e) => write!(f, "I/O エラー: {e}"),
            PlatformError::Trash(msg) => write!(f, "ゴミ箱送りに失敗しました: {msg}"),
            PlatformError::Unsupported(op) => write!(f, "この OS では未対応の操作です: {op}"),
        }
    }
}

impl std::error::Error for PlatformError {}

impl From<std::io::Error> for PlatformError {
    fn from(e: std::io::Error) -> Self {
        PlatformError::Io(e)
    }
}

/// `Platform` の操作結果。
pub type Result<T> = std::result::Result<T, PlatformError>;

/// OS 固有の知識（既知ディレクトリ解決・ゴミ箱送り・管理者権限要否判定）を
/// 隠蔽する抽象インターフェース（要件 4.2）。
///
/// 実装は `platform/windows.rs`（#4）・`platform/unknown.rs`（#4）に置く。
/// `core` 内の他のモジュールは本 trait 越しにのみ OS とやり取りする。
pub trait Platform {
    /// `kind` に対応する実パスを解決する。解決できない場合は `None`。
    fn known_dir(&self, kind: KnownDir) -> Option<PathBuf>;

    /// `path` をゴミ箱へ送る。
    fn to_trash(&self, path: &Path) -> Result<()>;

    /// `path` の削除に管理者権限が必要かどうかを判定する。
    fn requires_admin(&self, path: &Path) -> bool;

    /// アプリ設定の保存先ディレクトリ（Windows では `%APPDATA%\pc-cleaner`）。
    /// 解決できない場合は `None`（F-CFG-04）。
    fn config_dir(&self) -> Option<PathBuf>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Platform は &dyn Platform として core 内の他モジュール（#5/#7）に
    // 渡される想定のため、dyn 互換であることをコンパイル時に固定する。
    fn _assert_dyn(_: &dyn Platform) {}

    #[test]
    fn current_returns_a_platform_impl() {
        let _platform = current();
    }
}
