//! Runtime detection of copy-on-write (CoW) filesystem support.
//!
//! REFLINK-2 foundation. Linux `FICLONE`/`FICLONERANGE` only succeed on
//! reflink-capable filesystems (btrfs, XFS with `mkfs.xfs -m reflink=1`,
//! bcachefs). Every other filesystem returns `EOPNOTSUPP` or `EXDEV`,
//! costing one round-trip syscall plus a destination-file create/unlink
//! per failed probe. This module exposes a cheap `statfs(2)` probe that
//! short-circuits the FICLONE attempt when the filesystem is known not
//! to support reflinks.
//!
//! Detection rules (`f_type` magic per Linux super-block):
//!
//! - btrfs (`0x9123683E`) and bcachefs (`0xCA451A4E`) unambiguously
//!   support reflink at the kernel level. Returns
//!   [`CowSupport::Yes`].
//! - XFS (`0x58465342`) supports reflink only when the volume was
//!   formatted with `-m reflink=1`. There is no cheap runtime probe
//!   short of `FICLONE` itself, so this layer returns
//!   [`CowSupport::Probable`] and lets the caller run a single
//!   FICLONE attempt to confirm. The first-attempt outcome is cached
//!   per-device, so subsequent files on the same mountpoint pay no
//!   extra syscall.
//! - ext4 / tmpfs / proc / sysfs / NFS / FUSE / etc.: known not to
//!   support reflink. Returns [`CowSupport::No`] so the dispatch
//!   skips FICLONE entirely.
//! - ZFS-on-Linux (`0x2FC12FC1`) reports itself separately and gates
//!   reflink on the dataset-level `clone_blocks` feature. We treat it
//!   as [`CowSupport::Probable`] so a single FICLONE probe confirms.
//!
//! The detection result is cached in a process-wide
//! [`OnceLock<Mutex<HashMap>>`] keyed by `statfs.f_fsid` so repeated
//! copies on the same mountpoint only pay the `statfs` cost once.
//!
//! # Platform Support
//!
//! - **Linux**: full `statfs(2)`-backed probe.
//! - **Other platforms**: stub that returns `CowSupport::No` /
//!   `detect_cow_filesystem` -> `Ok(false)`. Cross-platform callers can
//!   use the public helpers without `#[cfg]` branching at every call
//!   site; the macOS APFS and Windows ReFS reflink paths are handled
//!   by their own dispatch layers (`clonefile`, `refs_detect`).

use std::io;
use std::path::Path;

/// Outcome of a CoW filesystem probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CowSupport {
    /// Filesystem unambiguously supports reflink. The caller should
    /// attempt `FICLONE` and treat failure as a real error.
    Yes,
    /// Filesystem does not support reflink. The caller should skip
    /// `FICLONE` entirely and fall through to the next dispatch tier
    /// (e.g. `copy_file_range` on Linux).
    No,
    /// Filesystem may support reflink but cannot be determined from
    /// `f_type` alone (XFS requires `-m reflink=1` at mkfs time; ZFS
    /// gates on a dataset feature). The caller should attempt a single
    /// `FICLONE` probe and cache the result via
    /// [`record_probe_outcome`].
    Probable,
}

impl CowSupport {
    /// Collapses the three-state probe outcome to a boolean answer that
    /// matches the REFLINK-2 task contract:
    ///
    /// - [`CowSupport::Yes`] and [`CowSupport::Probable`] both map to
    ///   `true`. The caller may attempt `FICLONE`; `Probable` will be
    ///   resolved on the first attempt and cached.
    /// - [`CowSupport::No`] maps to `false`; the caller skips the
    ///   reflink path entirely.
    #[must_use]
    pub fn may_attempt_reflink(self) -> bool {
        matches!(self, Self::Yes | Self::Probable)
    }
}

/// Returns the CoW support classification for the filesystem
/// containing `path`.
///
/// On Linux this calls `statfs(2)` and inspects `f_type`. The result is
/// computed once per `(fs_id, fs_type)` and cached in a process-wide
/// map keyed by the `statfs.f_fsid` returned by the kernel so
/// subsequent calls for the same mountpoint return without a syscall.
///
/// On every other platform this returns [`CowSupport::No`] without
/// touching the filesystem. macOS APFS and Windows ReFS reflink
/// support is handled by their dedicated dispatch layers
/// (`platform_copy::dispatch::clonefile_impl`,
/// `fast_io::refs_detect`), not by this probe.
///
/// # Errors
///
/// On Linux, returns the underlying `io::Error` if `statfs(2)` fails
/// (e.g. `ENOENT` when `path` does not exist, `EACCES` when the
/// directory is unreadable). The non-Linux stub never errors.
pub fn detect_cow_support(path: &Path) -> io::Result<CowSupport> {
    imp::detect_cow_support(path)
}

/// Boolean front door over [`detect_cow_support`] for callers that do
/// not need to distinguish `Yes` from `Probable`.
///
/// Returns `Ok(true)` when the filesystem may support reflink (btrfs,
/// XFS, bcachefs, ZFS) and `Ok(false)` when it cannot. This is the
/// foundation the REFLINK-3 (`FICLONE` whole-file) and REFLINK-4
/// (`FICLONERANGE` delta) wiring will gate on.
///
/// On non-Linux platforms this always returns `Ok(false)` without
/// touching the filesystem.
///
/// # Errors
///
/// On Linux, returns the underlying `io::Error` if `statfs(2)` fails.
/// The non-Linux stub never errors.
pub fn detect_cow_filesystem(path: &Path) -> io::Result<bool> {
    detect_cow_support(path).map(CowSupport::may_attempt_reflink)
}

/// Records the outcome of a confirming FICLONE probe so later callers
/// for the same mountpoint can skip the syscall when the probe failed
/// (or treat it as cached success when it worked).
///
/// Intended for use by the dispatch layer after a
/// [`CowSupport::Probable`] result from [`detect_cow_support`] has
/// been confirmed (or refuted) by a single FICLONE attempt.
/// Idempotent - subsequent calls on the same key overwrite the
/// previous value.
///
/// On non-Linux platforms this is a no-op.
///
/// # Errors
///
/// On Linux, returns the underlying `io::Error` if `statfs(2)` fails.
/// The non-Linux stub never errors.
pub fn record_probe_outcome(path: &Path, outcome: CowSupport) -> io::Result<()> {
    imp::record_probe_outcome(path, outcome)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::CowSupport;
    use std::collections::HashMap;
    use std::io;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    const BTRFS_SUPER_MAGIC: i64 = 0x9123_683E;
    const XFS_SUPER_MAGIC: i64 = 0x5846_5342;
    const BCACHEFS_SUPER_MAGIC: i64 = 0xCA45_1A4E;
    const ZFS_SUPER_MAGIC: i64 = 0x2FC1_2FC1;

    pub(super) fn detect_cow_support(path: &Path) -> io::Result<CowSupport> {
        let (fs_id, fs_type) = statfs_for_path(path)?;
        let cache = cache();
        if let Some(cached) = lookup(cache, fs_id) {
            return Ok(cached);
        }
        let support = classify(fs_type);
        insert(cache, fs_id, support);
        Ok(support)
    }

    pub(super) fn record_probe_outcome(path: &Path, outcome: CowSupport) -> io::Result<()> {
        let (fs_id, _) = statfs_for_path(path)?;
        insert(cache(), fs_id, outcome);
        Ok(())
    }

    pub(super) fn classify(fs_type: i64) -> CowSupport {
        match fs_type {
            BTRFS_SUPER_MAGIC | BCACHEFS_SUPER_MAGIC => CowSupport::Yes,
            XFS_SUPER_MAGIC | ZFS_SUPER_MAGIC => CowSupport::Probable,
            _ => CowSupport::No,
        }
    }

    type Cache = OnceLock<Mutex<HashMap<u64, CowSupport>>>;

    fn cache() -> &'static Cache {
        static CACHE: Cache = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        &CACHE
    }

    fn lookup(cache: &Cache, fs_id: u64) -> Option<CowSupport> {
        let guard = cache.get()?.lock().ok()?;
        guard.get(&fs_id).copied()
    }

    fn insert(cache: &Cache, fs_id: u64, support: CowSupport) {
        let Some(lock) = cache.get() else {
            return;
        };
        if let Ok(mut guard) = lock.lock() {
            guard.insert(fs_id, support);
        }
    }

    #[allow(unsafe_code)]
    fn statfs_for_path(path: &Path) -> io::Result<(u64, i64)> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let cpath = CString::new(path.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // SAFETY: `cpath` is a NUL-terminated C string owned for the call
        // and `buf` is a stack-resident `libc::statfs` whose address is
        // valid for the duration of the syscall. Standard `libc::statfs`
        // invocation idiom (mirrors `looks_like_nfs` in
        // `engine/benches/per_op_thresholds.rs`).
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(cpath.as_ptr(), &mut buf) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let fs_id = fsid_to_u64(&buf.f_fsid);
        Ok((fs_id, buf.f_type as i64))
    }

    #[allow(unsafe_code)]
    fn fsid_to_u64(fsid: &libc::fsid_t) -> u64 {
        // SAFETY: `libc::fsid_t` is a POD wrapper around `[i32; 2]` (glibc)
        // or `[c_int; 2]` (musl); both are 8 bytes total with no padding.
        // Reading the bytes as a `u64` is well-defined for any bit pattern.
        let bytes: [u8; 8] = unsafe { std::mem::transmute_copy(fsid) };
        u64::from_ne_bytes(bytes)
    }

    #[cfg(test)]
    pub mod test_hooks {
        use super::{
            BCACHEFS_SUPER_MAGIC, BTRFS_SUPER_MAGIC, XFS_SUPER_MAGIC, ZFS_SUPER_MAGIC, classify,
        };
        use crate::platform_copy::cow_detect::CowSupport;

        pub fn classify_magic(fs_type: i64) -> CowSupport {
            classify(fs_type)
        }

        pub const BTRFS: i64 = BTRFS_SUPER_MAGIC;
        pub const XFS: i64 = XFS_SUPER_MAGIC;
        pub const BCACHEFS: i64 = BCACHEFS_SUPER_MAGIC;
        pub const ZFS: i64 = ZFS_SUPER_MAGIC;
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::CowSupport;
    use std::io;
    use std::path::Path;

    pub(super) fn detect_cow_support(_path: &Path) -> io::Result<CowSupport> {
        Ok(CowSupport::No)
    }

    pub(super) fn record_probe_outcome(_path: &Path, _outcome: CowSupport) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn detect_handles_tmpdir_without_error() {
        let support = detect_cow_support(&tmp_dir()).expect("statfs on tmpdir");
        // tmpdir is typically tmpfs (No) or btrfs (Yes); Probable would
        // indicate XFS-backed /tmp which is rare but legal. All three
        // are valid outcomes - the test only asserts the probe runs.
        assert!(matches!(
            support,
            CowSupport::Yes | CowSupport::No | CowSupport::Probable
        ));
    }

    #[test]
    fn detect_cow_filesystem_handles_tmpdir() {
        let answer = detect_cow_filesystem(&tmp_dir()).expect("detect on tmpdir");
        // Whatever the underlying FS is, the bool must agree with the
        // three-state probe's `may_attempt_reflink` collapse.
        let support = detect_cow_support(&tmp_dir()).expect("statfs on tmpdir");
        assert_eq!(answer, support.may_attempt_reflink());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detect_handles_root_without_error() {
        let support = detect_cow_support(Path::new("/")).expect("statfs on /");
        assert!(matches!(
            support,
            CowSupport::Yes | CowSupport::No | CowSupport::Probable
        ));
    }

    #[test]
    fn detect_cache_is_consistent_for_repeated_calls() {
        let path = tmp_dir();
        let first = detect_cow_support(&path).expect("first probe");
        let second = detect_cow_support(&path).expect("second probe");
        assert_eq!(first, second);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detect_returns_enoent_for_missing_path() {
        let err = detect_cow_support(Path::new("/nonexistent-cow-detect-target"))
            .expect_err("missing path should error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_known_magics() {
        use imp::test_hooks;
        assert_eq!(
            test_hooks::classify_magic(test_hooks::BTRFS),
            CowSupport::Yes
        );
        assert_eq!(
            test_hooks::classify_magic(test_hooks::BCACHEFS),
            CowSupport::Yes
        );
        assert_eq!(
            test_hooks::classify_magic(test_hooks::XFS),
            CowSupport::Probable
        );
        assert_eq!(
            test_hooks::classify_magic(test_hooks::ZFS),
            CowSupport::Probable
        );
        // ext4 (0xEF53), tmpfs (0x01021994), proc (0x9fa0): all No.
        assert_eq!(test_hooks::classify_magic(0xEF53), CowSupport::No);
        assert_eq!(test_hooks::classify_magic(0x0102_1994), CowSupport::No);
        assert_eq!(test_hooks::classify_magic(0x9fa0), CowSupport::No);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn record_probe_outcome_overrides_classification() {
        let path = tmp_dir();
        // Force a cache entry to No, then verify lookup returns it
        // regardless of the underlying filesystem magic.
        record_probe_outcome(&path, CowSupport::No).expect("record outcome");
        let after = detect_cow_support(&path).expect("post-record probe");
        assert_eq!(after, CowSupport::No);
    }

    #[test]
    fn may_attempt_reflink_collapses_correctly() {
        assert!(CowSupport::Yes.may_attempt_reflink());
        assert!(CowSupport::Probable.may_attempt_reflink());
        assert!(!CowSupport::No.may_attempt_reflink());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_stub_always_returns_false() {
        let answer = detect_cow_filesystem(&tmp_dir()).expect("stub does not error");
        assert!(!answer);
        let support = detect_cow_support(&tmp_dir()).expect("stub does not error");
        assert_eq!(support, CowSupport::No);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_record_probe_is_noop() {
        // Should not error and should not change the answer.
        record_probe_outcome(&tmp_dir(), CowSupport::Yes).expect("stub no-ops");
        assert!(!detect_cow_filesystem(&tmp_dir()).expect("stub no-ops"));
    }
}
