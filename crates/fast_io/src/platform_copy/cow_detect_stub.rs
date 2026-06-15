//! Non-Linux stub for [`super::cow_detect`].
//!
//! macOS and Windows have their own CoW dispatch paths (`clonefile`,
//! `FSCTL_DUPLICATE_EXTENTS_TO_FILE`) with platform-specific runtime
//! detection elsewhere in this crate. This stub keeps the
//! cross-platform call signature uniform so callers in
//! `platform_copy::dispatch` do not need extra `#[cfg]` branches at
//! every site - the helper simply returns `No` so the Linux-only
//! FICLONE branch is skipped.

#![cfg(not(target_os = "linux"))]

use std::io;
use std::path::Path;

/// Stub mirror of the Linux [`super::cow_detect::CowSupport`] enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CowSupport {
    /// Unreachable on non-Linux but kept for API symmetry.
    Yes,
    /// Always returned on non-Linux platforms.
    No,
    /// Unreachable on non-Linux but kept for API symmetry.
    Probable,
}

/// Always returns [`CowSupport::No`] on non-Linux.
///
/// # Errors
///
/// Never errors on the stub path.
pub fn detect_cow_support(_path: &Path) -> io::Result<CowSupport> {
    Ok(CowSupport::No)
}

/// No-op on non-Linux.
///
/// # Errors
///
/// Never errors on the stub path.
pub fn record_probe_outcome(_path: &Path, _outcome: CowSupport) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_always_returns_no() {
        let support = detect_cow_support(Path::new("/")).expect("stub never errors");
        assert_eq!(support, CowSupport::No);
    }

    #[test]
    fn stub_record_outcome_is_noop() {
        record_probe_outcome(Path::new("/"), CowSupport::Yes).expect("stub never errors");
    }
}
