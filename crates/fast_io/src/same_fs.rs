//! Same-filesystem (device) detection for reflink / copy-on-write gating.
//!
//! Block-cloning fast paths (`FICLONE`, `FICLONERANGE`, `clonefile`, ReFS
//! `FSCTL_DUPLICATE_EXTENTS_TO_FILE`) only work when both operands reside on
//! the same filesystem device. Comparing the POSIX `st_dev` up front lets the
//! dispatch skip a doomed clone attempt (which would otherwise create the
//! destination, fail with `EXDEV`, and have to be cleaned up) on a
//! cross-device copy.
//!
//! Both helpers return `Option<bool>`:
//! - `Some(true)`  - the two operands share a device.
//! - `Some(false)` - the operands are on different devices.
//! - `None`        - device identity is unavailable (a metadata error, or a
//!   platform without a stable per-mount device id); the caller should treat
//!   this as "unknown" and let the clone attempt decide.

use std::fs::File;
use std::path::Path;

/// Returns whether two open files reside on the same filesystem device.
///
/// Compares the POSIX `st_dev` of each file. See the [module docs](self) for
/// the meaning of the returned `Option`.
#[cfg(unix)]
#[must_use]
pub fn files_same_device(a: &File, b: &File) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    match (a.metadata(), b.metadata()) {
        (Ok(x), Ok(y)) => Some(x.dev() == y.dev()),
        _ => None,
    }
}

/// Non-unix stub: device identity is not compared, so the result is `None`.
#[cfg(not(unix))]
#[must_use]
pub fn files_same_device(_a: &File, _b: &File) -> Option<bool> {
    None
}

/// Returns whether two paths reside on the same filesystem device.
///
/// Stats both paths and compares their POSIX `st_dev`. Use this when the
/// destination file does not exist yet: pass the destination's parent
/// directory, since the new file inherits its parent's device. See the
/// [module docs](self) for the meaning of the returned `Option`.
#[cfg(unix)]
#[must_use]
pub fn paths_same_device(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => Some(x.dev() == y.dev()),
        _ => None,
    }
}

/// Non-unix stub: device identity is not compared, so the result is `None`.
#[cfg(not(unix))]
#[must_use]
pub fn paths_same_device(_a: &Path, _b: &Path) -> Option<bool> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn two_files_in_one_dir_share_a_device() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"a").expect("write a");
        std::fs::write(&b, b"b").expect("write b");
        let fa = File::open(&a).expect("open a");
        let fb = File::open(&b).expect("open b");
        assert_eq!(files_same_device(&fa, &fb), Some(true));
        assert_eq!(paths_same_device(&a, &b), Some(true));
        // The parent-directory form (used before the destination exists)
        // agrees with the source file.
        assert_eq!(paths_same_device(&a, dir.path()), Some(true));
    }

    #[test]
    fn missing_path_yields_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let present = dir.path().join("present");
        std::fs::write(&present, b"x").expect("write");
        let missing = dir.path().join("missing");
        assert_eq!(paths_same_device(&present, &missing), None);
    }
}
