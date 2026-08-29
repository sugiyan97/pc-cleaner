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

/// 権限昇格（[`Platform::elevate`]）が成功しなかった理由（A2 / Issue #41）。
///
/// [`PlatformError`] とは別型にしている。`Cancelled` は「失敗」ではなく
/// ユーザーの正常な選択であり、I/O エラーと同じ扱いにすると呼び出し側が
/// 終了コードやメッセージを出し分けられなくなるため。
#[derive(Debug)]
pub enum ElevateError {
    /// UAC の確認画面でユーザーが「いいえ」を選んだ
    /// （Win32 の `ERROR_CANCELLED` / 1223）。
    Cancelled,
    /// 既に昇格済みのため昇格の必要がない。
    ///
    /// [`Platform::elevate`] は昇格済みならこれを返すだけで OS API を一切
    /// 呼ばない。無限に再昇格し続ける事故を防ぐための防御であり、CI
    /// （管理者権限で動くことがある `windows-latest` ランナー）でこの関数が
    /// 誤って呼ばれても実プロセスが生えないための安全策も兼ねる。
    AlreadyElevated,
    /// 自プロセスの実行ファイルパスを取得できなかった。
    CurrentExeUnavailable(String),
    /// OS が昇格を拒否した（グループポリシー等）。
    ///
    /// `windows::core::Error` を `core` の公開 API に漏らさないため、
    /// 実装側（`platform/windows.rs`）で翻訳済みの日本語メッセージだけを
    /// 持たせる（`PlatformError::Trash` と同じ方針）。
    Failed(String),
    /// この OS では昇格に対応していない（`unknown.rs` スタブ用、NF-OS-02）。
    Unsupported,
}

impl fmt::Display for ElevateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElevateError::Cancelled => write!(f, "管理者権限での実行はキャンセルされました。"),
            ElevateError::AlreadyElevated => write!(f, "既に管理者権限で実行されています。"),
            ElevateError::CurrentExeUnavailable(msg) => {
                write!(f, "実行ファイルの場所を特定できませんでした: {msg}")
            }
            ElevateError::Failed(msg) => write!(f, "管理者権限での起動に失敗しました: {msg}"),
            ElevateError::Unsupported => {
                write!(f, "この OS では管理者権限への昇格に対応していません。")
            }
        }
    }
}

impl std::error::Error for ElevateError {}

/// 権限昇格の操作結果。
pub type ElevateResult = std::result::Result<(), ElevateError>;

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

    /// 現在のプロセスが管理者権限（昇格済みトークン）で動作しているか
    /// （F-ELV-02 / A2 / Issue #41）。
    ///
    /// これはセキュリティ境界ではなく UI 上の判断材料である。実際の削除可否は
    /// 最終的に OS の ACL が決め、拒否された場合は `ItemOutcome::Failed` として
    /// 報告される。判定手段がない環境では安全側（`false` ＝昇格していない）
    /// に倒すこと（NF-SAF-01）。
    fn is_elevated(&self) -> bool;

    /// 自プロセスを管理者権限で起動し直す（F-ELV-01 / A2 / Issue #41）。
    ///
    /// `args` は新プロセスへ渡す引数列（`argv[0]` を含まない）。実行ファイルの
    /// パスは実装側が `std::env::current_exe()` から解決する。
    ///
    /// # 呼び出し側の責務
    /// `Ok(())` は「昇格プロセスの起動に成功した」ことだけを意味する。
    /// **呼び出し側は速やかに自プロセスを終了させること。** 終了しないと
    /// 同じ処理を行うプロセスが 2 つ同時に走ることになる。`core` 側では
    /// `process::exit` を呼ばない（GUI の後始末・テスト可能性を壊さないため）。
    fn elevate(&self, args: &[String]) -> ElevateResult;
}

/// 昇格して自プロセスを再実行すべきかを判定する純粋関数
/// （F-ELV-03 / A2 / Issue #41）。
///
/// `already_relaunched` は昇格後プロセスに渡される内部マーカー（CLI の
/// `--elevated` / GUI の同等の起動引数）。「昇格を要求したのに `is_elevated()`
/// が `false` のまま戻ってきた」環境で、無限に再昇格し続ける事故を防ぐ
/// 二重防御（[`Platform::elevate`] 自体の `AlreadyElevated` 早期リターンと
/// 合わせて二段構え）。
pub fn should_relaunch(admin_requested: bool, elevated: bool, already_relaunched: bool) -> bool {
    admin_requested && !elevated && !already_relaunched
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

    #[test]
    fn should_relaunch_decision_table() {
        // 昇格を要求していなければ、他の状態に関わらず再起動しない。
        assert!(!should_relaunch(false, false, false));
        assert!(!should_relaunch(false, true, false));
        assert!(!should_relaunch(false, false, true));
        assert!(!should_relaunch(false, true, true));

        // 要求あり・未昇格・未再起動のときだけ再起動する。
        assert!(should_relaunch(true, false, false));

        // 既に昇格済みなら再起動しない。
        assert!(!should_relaunch(true, true, false));
        assert!(!should_relaunch(true, true, true));

        // 昇格を要求して再起動したのに、まだ昇格していないと戻ってきた場合
        // （二重防御）でも、再度の再起動はしない＝無限ループにしない。
        assert!(!should_relaunch(true, false, true));
    }

    #[test]
    fn elevate_error_display_is_non_empty_japanese_text() {
        let variants = [
            ElevateError::Cancelled,
            ElevateError::AlreadyElevated,
            ElevateError::CurrentExeUnavailable("boom".to_string()),
            ElevateError::Failed("boom".to_string()),
            ElevateError::Unsupported,
        ];
        for variant in variants {
            let text = variant.to_string();
            assert!(!text.is_empty());
            assert!(!text.contains('{'), "Debug ダンプが漏れていないこと");
        }
    }
}
