//! Windows 以外の OS 向け最小スタブ（NF-OS-02）。
//!
//! 本ファイルは「対応 OS の実装が存在しないときのフォールバック」であり、
//! 走査・削除が成立しないことを明示的にエラーで返す。macOS / Linux の本対応
//! は将来対応 B1 / B2 とし、`platform/macos.rs` ・ `platform/linux.rs` の追加
//! で行う。その際は下の `#[cfg(not(windows))]` を
//! `#[cfg(not(any(windows, target_os = "macos")))]` のように狭めること。

#[cfg(not(windows))]
use super::{ElevateError, ElevateResult, KnownDir, Platform, PlatformError, Result};
#[cfg(not(windows))]
use std::path::{Path, PathBuf};

#[cfg(not(windows))]
pub(super) struct UnknownPlatform;

#[cfg(not(windows))]
impl UnknownPlatform {
    pub(super) fn new() -> Self {
        UnknownPlatform
    }
}

#[cfg(not(windows))]
impl Platform for UnknownPlatform {
    fn known_dir(&self, _kind: KnownDir) -> Option<PathBuf> {
        None
    }

    fn to_trash(&self, _path: &Path) -> Result<()> {
        Err(PlatformError::Unsupported("to_trash"))
    }

    fn requires_admin(&self, _path: &Path) -> bool {
        // 判定手段がないため、安全側（管理者権限が必要）に倒す（NF-SAF-01）。
        true
    }

    fn config_dir(&self) -> Option<PathBuf> {
        None
    }

    fn is_elevated(&self) -> bool {
        // 判定手段がないため、安全側（昇格していない）に倒す（NF-SAF-01）。
        false
    }

    fn elevate(&self, _args: &[String]) -> ElevateResult {
        Err(ElevateError::Unsupported)
    }
}

#[cfg(not(windows))]
pub(super) use self::UnknownPlatform as PlatformImpl;

#[cfg(test)]
#[cfg(not(windows))]
mod tests {
    use super::*;
    use crate::platform::RestoreItemOutcome;
    use std::path::Path;

    #[test]
    fn unknown_platform_is_unsupported_and_fails_safe() {
        let platform = UnknownPlatform::new();
        assert_eq!(platform.known_dir(KnownDir::UserTemp), None);
        assert_eq!(platform.known_dir(KnownDir::Downloads), None);
        assert!(matches!(
            platform.to_trash(Path::new("/tmp/foo")),
            Err(PlatformError::Unsupported("to_trash"))
        ));
        assert!(platform.requires_admin(Path::new("/tmp/foo")));
        assert_eq!(platform.config_dir(), None);
        assert!(!platform.is_elevated());
        assert!(matches!(
            platform.elevate(&[]),
            Err(ElevateError::Unsupported)
        ));
        assert!(matches!(
            platform.list_trash(),
            Err(PlatformError::Unsupported("list_trash"))
        ));
        let results = platform.restore_from_trash(&["a".to_string(), "b".to_string()]);
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|(_, outcome)| matches!(outcome, RestoreItemOutcome::Failed { .. }))
        );
    }
}
