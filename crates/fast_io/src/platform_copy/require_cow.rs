//! Adapter that requires a copy-on-write reflink for whole-file copies.
//!
//! Wraps the platform reflink primitives (`FICLONE`/`copy_file_range` on
//! Linux, `clonefile` on macOS, `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on
//! Windows ReFS) and surfaces the underlying error when the platform
//! reflink attempt fails instead of falling back to the portable
//! [`std::fs::copy`] loop. Selected by the
//! [`CowPolicy::Required`](crate::CowPolicy::Required) variant when
//! callers want a hard guarantee that destinations share blocks with
//! their sources (snapshot dedup, container layer builds).
//!
//! Mirrors the structure of [`NoZeroCopyPlatformCopy`](super::NoZeroCopyPlatformCopy)
//! and [`NoCowPlatformCopy`](super::NoCowPlatformCopy): the policy enum
//! lives in `lib.rs` and concrete adapters in `platform_copy/` swap into
//! the engine's [`LocalCopyOptions`](../../../engine/index.html) through
//! `with_platform_copy`.

use std::fmt;
use std::io;
use std::path::Path;

use super::dispatch;
use super::types::{CopyMethod, CopyResult, PlatformCopy};

/// `PlatformCopy` adapter that forces a CoW reflink and surfaces the
/// platform error when reflink is unavailable.
///
/// Used when [`CowPolicy::Required`](crate::CowPolicy::Required) is in
/// effect. Returns [`io::ErrorKind::Unsupported`] (or the underlying
/// platform error) instead of falling back to [`std::fs::copy`] so the
/// caller observes a hard failure when the destination filesystem cannot
/// honour the reflink request.
#[derive(Default)]
pub struct RequireCowPlatformCopy;

impl RequireCowPlatformCopy {
    /// Creates a new `RequireCowPlatformCopy` adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl fmt::Debug for RequireCowPlatformCopy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RequireCowPlatformCopy")
    }
}

impl PlatformCopy for RequireCowPlatformCopy {
    fn copy_file(&self, src: &Path, dst: &Path, _size_hint: u64) -> io::Result<CopyResult> {
        copy_via_reflink(src, dst)
    }

    fn supports_reflink(&self) -> bool {
        dispatch::platform_supports_reflink()
    }

    fn preferred_method(&self, _size: u64) -> CopyMethod {
        preferred_reflink_method()
    }
}

/// Linux: require `FICLONE`. No fallback to `copy_file_range` or
/// `std::fs::copy`.
#[cfg(target_os = "linux")]
fn copy_via_reflink(src: &Path, dst: &Path) -> io::Result<CopyResult> {
    match dispatch::try_ficlone_impl(src, dst) {
        Ok(()) => Ok(CopyResult::new(0, CopyMethod::Ficlone)),
        Err(err) => {
            let _ = std::fs::remove_file(dst);
            Err(err)
        }
    }
}

/// macOS: require `clonefile`. No fallback to `fcopyfile` or
/// `std::fs::copy`.
#[cfg(target_os = "macos")]
fn copy_via_reflink(src: &Path, dst: &Path) -> io::Result<CopyResult> {
    match dispatch::clonefile_impl(src, dst) {
        Ok(()) => Ok(CopyResult::new(0, CopyMethod::Clonefile)),
        Err(err) => {
            let _ = std::fs::remove_file(dst);
            Err(err)
        }
    }
}

/// Windows: require `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on a ReFS volume.
/// No fallback to `CopyFileExW` or `std::fs::copy`.
#[cfg(target_os = "windows")]
fn copy_via_reflink(src: &Path, dst: &Path) -> io::Result<CopyResult> {
    match dispatch::try_refs_reflink_impl(src, dst) {
        Ok(()) => Ok(CopyResult::new(0, CopyMethod::ReFsReflink)),
        Err(err) => {
            let _ = std::fs::remove_file(dst);
            Err(err)
        }
    }
}

/// Other platforms: reflink is not supported.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn copy_via_reflink(_src: &Path, _dst: &Path) -> io::Result<CopyResult> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "copy-on-write reflinks are not supported on this platform",
    ))
}

#[cfg(target_os = "linux")]
const fn preferred_reflink_method() -> CopyMethod {
    CopyMethod::Ficlone
}

#[cfg(target_os = "macos")]
const fn preferred_reflink_method() -> CopyMethod {
    CopyMethod::Clonefile
}

#[cfg(target_os = "windows")]
const fn preferred_reflink_method() -> CopyMethod {
    CopyMethod::ReFsReflink
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const fn preferred_reflink_method() -> CopyMethod {
    CopyMethod::StandardCopy
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(target_os = "windows"))]
    use tempfile::TempDir;

    #[test]
    fn debug_impl_is_concise() {
        let copier = RequireCowPlatformCopy::new();
        assert_eq!(format!("{copier:?}"), "RequireCowPlatformCopy");
    }

    #[test]
    fn preferred_method_matches_platform_reflink_primitive() {
        let copier = RequireCowPlatformCopy::new();
        assert_eq!(copier.preferred_method(0), preferred_reflink_method());
        assert_eq!(
            copier.preferred_method(64 * 1024 * 1024),
            preferred_reflink_method()
        );
    }

    /// On platforms without a reflink primitive, every copy must surface
    /// `Unsupported` instead of falling through to `std::fs::copy`.
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    #[test]
    fn copy_surfaces_unsupported_on_unsupported_platform() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"payload").unwrap();

        let copier = RequireCowPlatformCopy::new();
        let err = copier.copy_file(&src, &dst, 7).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(!dst.exists());
    }

    /// When a reflink primitive exists but the underlying filesystem
    /// does not support it (the common /tmp + tmpfs case on Linux CI
    /// and most non-APFS macOS test runners), the adapter must surface
    /// the platform error rather than silently completing via the
    /// portable copy path. Smoke-test the failure mode opportunistically
    /// so we do not depend on a specific filesystem being mounted.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn copy_either_clones_or_surfaces_error() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"payload").unwrap();

        let copier = RequireCowPlatformCopy::new();
        match copier.copy_file(&src, &dst, 7) {
            Ok(result) => {
                // Reflink succeeded: destination must mirror source.
                assert_eq!(result.method, preferred_reflink_method());
                assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
            }
            Err(_) => {
                // Reflink failed: destination must not exist (the adapter
                // cleans up the half-created file).
                assert!(!dst.exists());
            }
        }
    }
}
