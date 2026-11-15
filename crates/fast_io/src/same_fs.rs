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

/// Returns a stable per-volume identity for the filesystem that contains
/// `path`, or `None` when it cannot be determined.
///
/// This is the cross-platform analog of the POSIX `st_dev` that
/// `--one-file-system` (`-x`) compares to detect a mount/volume boundary:
///
/// - **Unix**: the `st_dev` of `path` (following symlinks, like `stat(2)`), so
///   a directory that is a mount point reports the mounted filesystem's device.
/// - **Windows**: the volume serial number reported by
///   `GetFileInformationByHandle`. Native Windows has no `st_dev`; the volume
///   serial is a per-volume identity that differs across a junction or a volume
///   mounted at a directory - a boundary a drive-letter path prefix cannot see.
///   The handle is opened with `FILE_FLAG_BACKUP_SEMANTICS` (so a directory can
///   be opened) and *without* `FILE_FLAG_OPEN_REPARSE_POINT` (so a junction
///   resolves to its target volume, matching the Unix follow-symlink `stat`).
/// - **Other platforms**: `None`.
///
/// upstream: flist.c uses `st.st_dev != filesystem_dev` to skip mount-point
/// dirs. Native Windows lacks `st_dev`, so this mirrors the *semantic* (do not
/// cross a filesystem/volume boundary) with the closest stable volume identity.
#[must_use]
pub fn volume_id(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| m.dev())
    }

    #[cfg(windows)]
    {
        windows_file_info(path, true)
            .ok()
            .map(|info| u64::from(info.dwVolumeSerialNumber))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// Stable per-file identity (volume, file index/inode, hard-link count).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileIdentity {
    /// Volume / device identifier (`st_dev` on Unix, `dwVolumeSerialNumber` on Windows).
    pub volume_id: u64,
    /// File index / inode number (`st_ino` on Unix, 64-bit `nFileIndex` on Windows).
    pub file_index: u64,
    /// Hard-link count (`st_nlink` on Unix, `nNumberOfLinks` on Windows).
    pub nlink: u32,
}

/// Returns the symlink-non-following [`FileIdentity`] for `path`, or `None`
/// when unavailable.
///
/// Mirrors `lstat(2)` on Unix and `GetFileInformationByHandle` with
/// `FILE_FLAG_OPEN_REPARSE_POINT` on Windows.
#[must_use]
pub fn file_identity(path: &Path) -> Option<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(FileIdentity {
            volume_id: meta.dev(),
            file_index: meta.ino(),
            nlink: meta.nlink().try_into().unwrap_or(u32::MAX),
        })
    }

    #[cfg(windows)]
    {
        let info = windows_file_info(path, false).ok()?;
        let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        Some(FileIdentity {
            volume_id: u64::from(info.dwVolumeSerialNumber),
            file_index,
            nlink: info.nNumberOfLinks,
        })
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// Windows helper: opens `path` and returns its `BY_HANDLE_FILE_INFORMATION`.
///
/// When `follow_reparse` is `true`, junctions and symlinks are followed to
/// their target (matching `stat(2)` for `-x`); when `false`, the reparse
/// point itself is queried (matching `lstat(2)` for hard-link identity).
#[cfg(windows)]
fn windows_file_info(
    path: &Path,
    follow_reparse: bool,
) -> std::io::Result<windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetFileInformationByHandle,
    };

    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if !follow_reparse {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }

    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)?;

    // SAFETY: `BY_HANDLE_FILE_INFORMATION` is plain-old-data with no invalid
    // bit patterns, so a zeroed value is valid before the call fills it. `file`
    // owns a valid open handle for the duration of the call, and
    // `GetFileInformationByHandle` only writes into `info`, returning 0 on
    // failure.
    #[allow(unsafe_code)]
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(info)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn two_files_in_one_dir_share_a_device() {
        let dir = test_support::create_tempdir();
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
        let dir = test_support::create_tempdir();
        let present = dir.path().join("present");
        std::fs::write(&present, b"x").expect("write");
        let missing = dir.path().join("missing");
        assert_eq!(paths_same_device(&present, &missing), None);
    }
}

#[cfg(all(test, any(unix, windows)))]
mod volume_tests {
    use super::*;

    #[test]
    fn volume_id_agrees_for_a_file_and_its_parent() {
        // A file and the directory that contains it always live on the same
        // volume, so their identities must match on every platform that
        // resolves one (all CI targets: Linux, macOS, Windows).
        let dir = test_support::create_tempdir();
        let file = dir.path().join("probe");
        std::fs::write(&file, b"x").expect("write probe");

        let dir_id = volume_id(dir.path());
        assert!(dir_id.is_some(), "temp dir must resolve a volume id here");
        assert_eq!(dir_id, volume_id(&file));
    }

    #[test]
    fn volume_id_missing_path_is_none() {
        let dir = test_support::create_tempdir();
        assert_eq!(volume_id(&dir.path().join("does-not-exist")), None);
    }

    #[test]
    fn file_identity_matches_for_hard_links() {
        let dir = test_support::create_tempdir();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"data").unwrap();
        std::fs::hard_link(&a, &b).unwrap();

        let id_a = file_identity(&a).expect("file_identity(a)");
        let id_b = file_identity(&b).expect("file_identity(b)");
        assert_eq!(id_a.volume_id, id_b.volume_id);
        assert_eq!(id_a.file_index, id_b.file_index);
        assert_eq!(id_a.nlink, 2);
        assert_eq!(id_b.nlink, 2);
    }
}
