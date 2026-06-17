//! Runtime detection of copy-on-write (CoW) filesystem support on Linux.
//!
//! REFLINK-2 helper. Linux `FICLONE`/`FICLONERANGE` only succeed on
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
//!
//! The detection result is cached in a process-wide
//! [`OnceLock<Mutex<HashMap>>`] keyed by the `(dev_major, dev_minor)`
//! tuple from `statfs.f_fsid` so repeated copies on the same mount
//! only pay the `statfs` cost once.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

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
    /// `f_type` alone (XFS requires `-m reflink=1` at mkfs time).
    /// The caller should attempt a single `FICLONE` probe and cache
    /// the result.
    Probable,
}

const BTRFS_SUPER_MAGIC: i64 = 0x9123_683E;
const XFS_SUPER_MAGIC: i64 = 0x5846_5342;
const BCACHEFS_SUPER_MAGIC: i64 = 0xCA45_1A4E;

/// Returns the CoW support classification for the filesystem
/// containing `path`.
///
/// Calls `statfs(2)` and inspects `f_type`. The result is computed
/// once per `(dev, fs_type)` and cached in a process-wide map keyed
/// by the `statfs.f_fsid` returned by the kernel so subsequent calls
/// for the same mountpoint return without a syscall.
///
/// # Errors
///
/// Returns the underlying `io::Error` if `statfs(2)` fails (e.g.
/// `ENOENT` when `path` does not exist, `EACCES` when the directory
/// is unreadable).
pub fn detect_cow_support(path: &Path) -> io::Result<CowSupport> {
    let (fs_id, fs_type) = statfs_for_path(path)?;
    let cache = cache();
    if let Some(cached) = lookup(cache, fs_id) {
        return Ok(cached);
    }
    let support = classify(fs_type);
    insert(cache, fs_id, support);
    Ok(support)
}

/// Records the outcome of a confirming FICLONE probe so later callers
/// for the same mountpoint can skip the syscall when the probe
/// failed (or treat it as cached success when it worked).
///
/// Intended for use by the dispatch layer after a `Probable` result
/// from [`detect_cow_support`] has been confirmed (or refuted) by a
/// single FICLONE attempt. Idempotent - subsequent calls on the same
/// key overwrite the previous value.
///
/// # Errors
///
/// Returns the underlying `io::Error` if `statfs(2)` fails.
pub fn record_probe_outcome(path: &Path, outcome: CowSupport) -> io::Result<()> {
    let (fs_id, _) = statfs_for_path(path)?;
    insert(cache(), fs_id, outcome);
    Ok(())
}

fn classify(fs_type: i64) -> CowSupport {
    match fs_type {
        BTRFS_SUPER_MAGIC | BCACHEFS_SUPER_MAGIC => CowSupport::Yes,
        XFS_SUPER_MAGIC => CowSupport::Probable,
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

    #[test]
    fn detect_returns_enoent_for_missing_path() {
        let err = detect_cow_support(Path::new("/nonexistent-cow-detect-target"))
            .expect_err("missing path should error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn classify_known_magics() {
        assert_eq!(classify(BTRFS_SUPER_MAGIC), CowSupport::Yes);
        assert_eq!(classify(BCACHEFS_SUPER_MAGIC), CowSupport::Yes);
        assert_eq!(classify(XFS_SUPER_MAGIC), CowSupport::Probable);
        // ext4 (0xEF53), tmpfs (0x01021994), proc (0x9fa0): all No.
        assert_eq!(classify(0xEF53), CowSupport::No);
        assert_eq!(classify(0x0102_1994), CowSupport::No);
        assert_eq!(classify(0x9fa0), CowSupport::No);
    }

    #[test]
    fn record_probe_outcome_overrides_classification() {
        let path = tmp_dir();
        // Force a cache entry to No, then verify lookup returns it
        // regardless of the underlying filesystem magic.
        record_probe_outcome(&path, CowSupport::No).expect("record outcome");
        let after = detect_cow_support(&path).expect("post-record probe");
        assert_eq!(after, CowSupport::No);
    }
}
