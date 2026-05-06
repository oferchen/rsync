//! Adapter that forces a non-zero-copy fallback for whole-file copies.
//!
//! Wraps [`DefaultPlatformCopy`](super::DefaultPlatformCopy) but replaces any
//! kernel zero-copy mechanism (`copy_file_range`, `sendfile`, `clonefile`,
//! `fcopyfile`, `FICLONE`, ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE`) with a
//! portable buffered [`std::fs::copy`] loop. Selected by the
//! [`ZeroCopyPolicy::Disabled`](crate::ZeroCopyPolicy::Disabled) variant
//! when callers want to disable I/O-level zero-copy optimizations.
//!
//! This adapter mirrors the pattern used by other policy gates: the policy
//! enum lives in `lib.rs`, and concrete adapters in `platform_copy/` swap
//! into the engine's [`LocalCopyOptions`](../../../engine/index.html)
//! through `with_platform_copy`.

use std::fmt;
use std::io;
use std::path::Path;

use super::types::{CopyMethod, CopyResult, PlatformCopy};

/// `PlatformCopy` adapter that forces standard buffered copy regardless of
/// what the underlying platform optimization chain would have selected.
///
/// Used when [`ZeroCopyPolicy::Disabled`](crate::ZeroCopyPolicy::Disabled)
/// is in effect. Reports [`CopyMethod::StandardCopy`] and `supports_reflink()
/// == false` so downstream code paths cannot accidentally re-enter a
/// kernel-side zero-copy mechanism through the trait surface.
#[derive(Default)]
pub struct NoZeroCopyPlatformCopy;

impl NoZeroCopyPlatformCopy {
    /// Creates a new `NoZeroCopyPlatformCopy` adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl fmt::Debug for NoZeroCopyPlatformCopy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NoZeroCopyPlatformCopy")
    }
}

impl PlatformCopy for NoZeroCopyPlatformCopy {
    fn copy_file(&self, src: &Path, dst: &Path, _size_hint: u64) -> io::Result<CopyResult> {
        let bytes = std::fs::copy(src, dst)?;
        Ok(CopyResult::new(bytes, CopyMethod::StandardCopy))
    }

    fn supports_reflink(&self) -> bool {
        false
    }

    fn preferred_method(&self, _size: u64) -> CopyMethod {
        CopyMethod::StandardCopy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn reports_standard_copy_method() {
        let copier = NoZeroCopyPlatformCopy::new();
        assert_eq!(copier.preferred_method(0), CopyMethod::StandardCopy);
        assert_eq!(
            copier.preferred_method(64 * 1024 * 1024),
            CopyMethod::StandardCopy
        );
    }

    #[test]
    fn does_not_advertise_reflink() {
        let copier = NoZeroCopyPlatformCopy::new();
        assert!(!copier.supports_reflink());
    }

    #[test]
    fn copies_file_via_standard_copy() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"hello world").unwrap();

        let copier = NoZeroCopyPlatformCopy::new();
        let result = copier.copy_file(&src, &dst, 11).unwrap();

        assert_eq!(result.method, CopyMethod::StandardCopy);
        assert_eq!(result.bytes_copied, 11);
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello world");
        assert!(!result.is_zero_copy());
    }

    #[test]
    fn debug_impl_is_concise() {
        let copier = NoZeroCopyPlatformCopy::new();
        assert_eq!(format!("{copier:?}"), "NoZeroCopyPlatformCopy");
    }
}
