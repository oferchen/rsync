//! Source-file opening with the upstream sender's symlink-race defences.
//!
//! The sender opens every source file through [`SourceOpen::open`], which
//! mirrors the branch upstream `sender.c` takes before reading a file:
//!
//! - **Daemon, non-chroot** (`use_secure_symlinks`): the file is opened
//!   confined beneath the module root via [`fast_io::open_source_confined`],
//!   so a swapped directory symlink cannot redirect the read outside the
//!   module (TOCTOU escape). upstream: `sender.c:secure_relative_open`.
//! - **Everything else** (`do_open_checklinks`): the file is opened with
//!   `O_NOFOLLOW` on the leaf so a symlinked final component is refused,
//!   UNLESS `--copy-links` / `--copy-unsafe-links` is set (then the symlink
//!   is followed with a plain open). upstream: `sender.c:do_open_checklinks`.
//!
//! Every open also honours `--open-noatime` (rsync 3.4.2): on Linux/Android
//! the open adds `O_NOATIME`, falling back to a plain open if the kernel or
//! filesystem rejects the flag (`EPERM`/`EACCES`/`EINVAL`/`ENOTSUP`/`EROFS`).
//! upstream: `syscall.c:228 do_open` / `syscall.c:687 do_open_nofollow`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(any(target_os = "linux", target_os = "android"))]
use libc::{EACCES, EINVAL, ENOTSUP, EPERM, EROFS, O_NOATIME};

/// The sender's per-connection source-open policy.
///
/// Built once from the transfer config (see
/// [`GeneratorContext::source_open`](crate::generator::GeneratorContext::source_open))
/// and shared by every source open so the confinement / `O_NOFOLLOW`
/// decision lives in exactly one place rather than being duplicated across
/// the delta, whole-file, and append read paths.
#[derive(Clone, Debug)]
pub(crate) struct SourceOpen {
    /// `Some(module_root)` for a non-chroot daemon sender: opens are
    /// confined beneath this absolute root. Unused on non-Unix (the daemon
    /// is Unix-only), hence the `dead_code` allowance there.
    #[cfg_attr(not(unix), allow(dead_code))]
    confine_root: Option<PathBuf>,
    /// Whether to follow a symlinked leaf (`--copy-links` /
    /// `--copy-unsafe-links`). When false, the leaf is opened `O_NOFOLLOW`.
    follow_symlinks: bool,
    /// Whether to propagate `O_NOATIME` (`--open-noatime`).
    noatime: bool,
}

impl SourceOpen {
    /// Builds a policy from its three inputs.
    pub(crate) fn new(confine_root: Option<PathBuf>, follow_symlinks: bool, noatime: bool) -> Self {
        Self {
            confine_root,
            follow_symlinks,
            noatime,
        }
    }

    /// Opens `path` under this policy.
    ///
    /// upstream: sender.c - `secure_relative_open` (daemon, confined) vs
    /// `do_open_checklinks` (`O_NOFOLLOW` leaf unless copy-links).
    pub(crate) fn open(&self, path: &Path) -> io::Result<fs::File> {
        #[cfg(unix)]
        if let Some(root) = self.confine_root.as_deref() {
            // upstream: sender.c:secure_relative_open - reconstruct the
            // module-relative path and open it confined beneath the module
            // root. oc keeps absolute source paths, so the module-relative
            // component is the source path with the module root stripped.
            if let Ok(relative) = path.strip_prefix(root) {
                return fast_io::open_source_confined(root, relative, self.noatime);
            }
            // A source path that is not beneath the module root should never
            // happen for a legitimate daemon transfer; fail safe by falling
            // through to the O_NOFOLLOW open rather than following symlinks.
        }

        if self.follow_symlinks {
            // upstream: sender.c:do_open_checklinks - copy_links /
            // copy_unsafe_links follow the symlink via a plain do_open.
            open_source_with_noatime(path, self.noatime)
        } else {
            // upstream: sender.c:do_open_checklinks -> do_open_nofollow.
            open_source_nofollow(path, self.noatime)
        }
    }
}

/// Opens a source file for reading following symlinks, honouring
/// `--open-noatime`.
///
/// upstream: syscall.c do_open (3.4.2 propagates `O_NOATIME` via the
/// `open_noatime` global).
pub(super) fn open_source_with_noatime(path: &Path, use_noatime: bool) -> io::Result<fs::File> {
    if use_noatime {
        if let Some(file) = try_open_noatime(path)? {
            return Ok(file);
        }
    }
    fs::File::open(path)
}

/// Opens a source file for reading with `O_NOFOLLOW` on the leaf so a
/// symlinked final component is refused with `ELOOP`, honouring
/// `--open-noatime`.
///
/// upstream: syscall.c do_open_nofollow - `open(pathname, flags|O_NOFOLLOW)`,
/// with `O_NOATIME` added when `open_noatime` is set.
#[cfg(unix)]
pub(super) fn open_source_nofollow(path: &Path, use_noatime: bool) -> io::Result<fs::File> {
    let nofollow = libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if use_noatime {
        if let Some(extra) = noatime_flag() {
            let mut options = fs::OpenOptions::new();
            options.read(true).custom_flags(nofollow | extra);
            match options.open(path) {
                Ok(file) => return Ok(file),
                Err(error) if noatime_retryable(&error) => {}
                Err(error) => return Err(error),
            }
        }
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(nofollow);
    options.open(path)
}

/// Non-Unix stub: `O_NOFOLLOW` is a Unix concept, so the leaf-symlink guard
/// degrades to the plain source open (matching the upstream limitation that
/// its `do_open_nofollow` guarantee is `O_NOFOLLOW`-platform only).
#[cfg(not(unix))]
pub(super) fn open_source_nofollow(path: &Path, use_noatime: bool) -> io::Result<fs::File> {
    open_source_with_noatime(path, use_noatime)
}

/// Returns `O_NOATIME` on Linux/Android, `None` on other Unix targets where
/// the flag is undefined.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn noatime_flag() -> Option<i32> {
    Some(O_NOATIME)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn noatime_flag() -> Option<i32> {
    None
}

/// Whether an `O_NOATIME` open failure should be retried without the flag.
#[cfg(unix)]
fn noatime_retryable(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EPERM | libc::EACCES | libc::EINVAL | libc::ENOTSUP | libc::EROFS)
    )
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn try_open_noatime(path: &Path) -> io::Result<Option<fs::File>> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(O_NOATIME);
    match options.open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) => match error.raw_os_error() {
            Some(EPERM | EACCES | EINVAL | ENOTSUP | EROFS) => Ok(None),
            _ => Err(error),
        },
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn try_open_noatime(_path: &Path) -> io::Result<Option<fs::File>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn open_source_with_noatime_disabled_returns_readable_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        std::fs::write(&path, b"payload").unwrap();

        let mut file = open_source_with_noatime(&path, false).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"payload");
    }

    #[test]
    fn open_source_with_noatime_enabled_returns_readable_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        std::fs::write(&path, b"payload").unwrap();

        // On non-Linux this falls through to plain File::open; on Linux it
        // either succeeds via O_NOATIME or falls back on EPERM/EROFS/etc.
        let mut file = open_source_with_noatime(&path, true).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"payload");
    }

    /// Linux-only regression test for upstream 3.4.2 `O_NOATIME` parity.
    ///
    /// Backdates the source file's atime, reads through
    /// `open_source_with_noatime(_, true)`, and confirms the on-disk atime
    /// did not advance. Skips silently when the host filesystem rejects
    /// `O_NOATIME` (tmpfs in restricted containers; some overlayfs setups).
    #[cfg(target_os = "linux")]
    #[test]
    fn open_source_with_noatime_preserves_atime_on_linux() {
        use std::os::unix::fs::MetadataExt;
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        std::fs::write(&path, b"payload-for-atime-check").unwrap();

        // Backdate atime to 1 hour ago so a fresh access would be visible.
        let past = SystemTime::now() - Duration::from_secs(3600);
        let past_ft = filetime::FileTime::from_system_time(past);
        filetime::set_file_atime(&path, past_ft).unwrap();

        let before = std::fs::metadata(&path).unwrap().atime();

        // Skip if the open returns success via the EPERM/EROFS fallback
        // path rather than via the O_NOATIME open.
        let mut options = fs::OpenOptions::new();
        options.read(true).custom_flags(O_NOATIME);
        if options.open(&path).is_err() {
            eprintln!("skip: O_NOATIME not honoured by this filesystem");
            return;
        }

        let mut file = open_source_with_noatime(&path, true).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        drop(file);

        let after = std::fs::metadata(&path).unwrap().atime();
        assert_eq!(
            before, after,
            "O_NOATIME should preserve atime (before={before}, after={after})"
        );
    }

    /// A non-daemon default policy (`O_NOFOLLOW`, no confinement) must refuse a
    /// source whose final component was swapped for a symlink.
    ///
    /// WHY: mirrors upstream `sender.c:do_open_checklinks -> do_open_nofollow`,
    /// which opens the source `O_NOFOLLOW` so a TOCTOU leaf-swap cannot
    /// redirect the read to an attacker-chosen file. A regression that dropped
    /// the flag would silently follow the swap and leak the target's contents.
    #[cfg(unix)]
    #[test]
    fn nofollow_policy_refuses_symlinked_leaf() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, b"do-not-leak").unwrap();
        let leaf = dir.path().join("leaf");
        symlink(&secret, &leaf).unwrap();

        let policy = SourceOpen::new(None, false, false);
        let err = policy
            .open(&leaf)
            .expect_err("O_NOFOLLOW must refuse a symlinked leaf");
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
    }

    /// With `--copy-links` / `--copy-unsafe-links` the policy follows the
    /// symlinked leaf, matching upstream `do_open_checklinks`'s plain `do_open`
    /// branch.
    #[cfg(unix)]
    #[test]
    fn follow_policy_opens_symlinked_leaf() {
        use std::io::Read as _;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"followed").unwrap();
        let leaf = dir.path().join("leaf");
        symlink(&target, &leaf).unwrap();

        let policy = SourceOpen::new(None, true, false);
        let mut file = policy.open(&leaf).expect("copy-links follows the leaf");
        let mut buf = String::new();
        file.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "followed");
    }

    /// A daemon policy confined beneath the module root must refuse an open
    /// whose directory component is a symlink escaping the module.
    ///
    /// WHY: mirrors upstream `sender.c:secure_relative_open`. Without
    /// confinement a module operator could plant `<module>/escape ->
    /// /outside` and pull any file the daemon can read.
    #[cfg(unix)]
    #[test]
    fn daemon_confined_policy_refuses_escape() {
        use std::os::unix::fs::symlink;

        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("module");
        std::fs::create_dir(&root).unwrap();
        let outside = base.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"do-not-leak").unwrap();
        symlink(&outside, root.join("escape")).unwrap();

        let policy = SourceOpen::new(Some(root.clone()), false, false);
        // The sender passes the reconstructed absolute source path; the policy
        // strips the module root and opens the remainder confined.
        let err = policy
            .open(&root.join("escape/secret"))
            .expect_err("confined open must refuse the escape");
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::EXDEV) || code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "expected EXDEV, ELOOP, or ENOTDIR for a confined escape, got: {err}"
        );
    }

    /// The confined daemon policy still opens a legitimate in-module file.
    #[cfg(unix)]
    #[test]
    fn daemon_confined_policy_opens_in_module_file() {
        use std::io::Read as _;

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        std::fs::write(root.path().join("sub/data"), b"in-module").unwrap();

        let policy = SourceOpen::new(Some(root.path().to_path_buf()), false, false);
        let mut file = policy
            .open(&root.path().join("sub/data"))
            .expect("in-module source must open");
        let mut buf = String::new();
        file.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "in-module");
    }
}
