//! Source-scan filesystem lookups anchored on the pinned session root.
//!
//! Upstream's sender never names the module by its absolute path after the
//! daemon drops privilege. It `change_dir()`s into the module root while still
//! privileged, pins that directory as `module_dirfd`
//! (`clientserver.c:1059-1065`), and every later lookup is either relative to
//! the resulting cwd or explicitly anchored on that descriptor
//! (`flist.c:2035-2059` for the scan, `sender.c:293-295` for the content
//! open). oc keeps source paths absolute instead, which is fine until the
//! module sits under a directory the dropped uid cannot search: then every
//! `stat`/`opendir` on an absolute in-module path re-traverses that ancestor
//! and fails with `EACCES`, even though the module itself is world-readable.
//!
//! These helpers restore upstream's position without giving up absolute paths.
//! Each one asks
//! [`pinned_root_relative`](crate::confinement::pinned_root_relative) whether
//! the path lies beneath the pinned root; if it does the lookup is issued
//! `*at`-style against the pinned descriptor, and if it does not - no daemon,
//! no pin, or a path outside the module - it is the ordinary `std::fs` call it
//! has always been. The two arms are the same lookup of the same directory
//! entry; only the starting point differs.
//!
//! # What makes the pinned answer trustworthy
//!
//! Answering `lstat(<module root>)` from the pin means answering it about the
//! directory the pin names, so the pin has to be the directory the module
//! resolves to and nothing else. It is:
//! [`pin_session_root_fd`](crate::confinement::pin_session_root_fd) opens the
//! root through the ownership walk, which is what upstream's `change_dir()`
//! does for a non-chrooted daemon (`util1.c:1254-1263`), so a symlink an
//! attacker planted at a component of the configured `path =` is refused and
//! no pin is taken at all. Every helper here then falls back to the absolute
//! path, which is where the sender's own foreign-symlink refusal still lives.
//!
//! # Platform
//!
//! The directory scan anchors on every Unix - `openat(O_RDONLY|O_DIRECTORY)`
//! plus `fdopendir` is portable. The *stat* is Linux-only, because anchoring
//! it needs an open that reports a directory entry's metadata without
//! requiring read access to it and without following a symlinked leaf, and
//! `O_PATH` is the only open that does both. Elsewhere the stat takes the
//! ordinary path-based arm, which is what it did before this module existed -
//! the anchoring is an added capability, never a weakened one, so a target
//! without it is exactly as confined as it was.
//!
//! # Upstream Reference
//!
//! - `clientserver.c:1059-1065` - the pin, taken before the privilege drop.
//! - `flist.c:2028-2059` `secure_opendir()` - the scan anchored on it.
//! - `flist.c:1362-1370` `link_stat()` - the per-entry stat the scan drives.

use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

/// `lstat` for a source path, anchored on the pinned root when it applies.
///
/// Equivalent to [`std::fs::symlink_metadata`] in every observable way; see
/// the module docs for what "anchored" buys.
///
/// # Errors
///
/// The underlying `lstat`/`openat` error.
pub fn symlink_metadata(path: &Path) -> io::Result<Metadata> {
    match anchored_metadata(path, false) {
        Some(result) => result,
        None => std::fs::symlink_metadata(path),
    }
}

/// `stat` for a source path, anchored on the pinned root when it applies.
///
/// Equivalent to [`std::fs::metadata`] in every observable way.
///
/// # Errors
///
/// The underlying `stat`/`openat` error.
pub fn metadata(path: &Path) -> io::Result<Metadata> {
    match anchored_metadata(path, true) {
        Some(result) => result,
        None => std::fs::metadata(path),
    }
}

/// `opendir` + `readdir` for a source directory, anchored on the pinned root
/// when it applies.
///
/// The iterator yields absolute child paths built by joining each name onto
/// `path`, so callers see exactly what [`std::fs::read_dir`] would have given
/// them. `.` and `..` are skipped by both arms.
///
/// # Errors
///
/// The `opendir` error, reported here; per-entry errors are reported by the
/// iterator, matching [`std::fs::ReadDir`].
pub fn read_dir(path: &Path) -> io::Result<ReadDir> {
    #[cfg(unix)]
    if let Some((fd, relative)) = crate::confinement::pinned_root_relative(path) {
        use std::os::fd::AsFd;
        let dir = imp::open_dir_at(fd.as_fd(), &relative)?;
        let names = crate::confined_readdir::read_dir_names_at(dir.as_fd())?;
        return Ok(ReadDir(Source::Names {
            dir: path.to_path_buf(),
            names: names.into_iter(),
        }));
    }
    Ok(ReadDir(Source::Std(std::fs::read_dir(path)?)))
}

/// Like [`read_dir`], but anchored on a root the CALLER supplies instead of the
/// ambient session pin.
///
/// [`read_dir`] reads its anchor from the process-global session root, which is
/// only ever correct when the process serves one confinement domain at a time -
/// upstream's situation, because it forks a child per connection. oc serves
/// each daemon connection on a worker thread of one process, so a per-connection
/// root cannot live in a global without two concurrent connections overwriting
/// each other's boundary. Passing the root in makes the anchor a parameter of
/// the call rather than ambient state, which is what keeps it correct under
/// threads.
///
/// The walk beneath `root` is the confined one: an absolute symlink target, a
/// `..` above the anchor, or an excluded component is REFUSED rather than
/// followed, so a refused scan can never be mistaken for an empty directory.
///
/// ⚠ `strip_prefix` is LEXICAL, so "does not lie beneath `root`" here means
/// NOT ANCHORABLE, never "escapes the root". A relative operand resolves
/// against the process cwd - which is typically inside the root, since that is
/// how a restricted shell invokes rsync - and an absolute path can be spelled
/// differently from the root it is under. Both fall back to the ordinary read,
/// exactly as [`crate::confinement::pinned_root_relative`] already decides for
/// the ambient pin: "Returns `None` - meaning resolve `path` the ordinary way -
/// when no root is pinned, or when `path` does not lie beneath the pinned
/// root."
///
/// Treating a failed `strip_prefix` as an escape and refusing would be the same
/// error this anchoring exists to fix: a lexical test standing in for a
/// resolution decision. It refuses directories that were never outside
/// anything - measured, as `opendir "sub" failed: Cross-device link`.
///
/// # Errors
///
/// - The walk's refusal (`ELOOP` for an absolute or escaping symlink target,
///   `..` above the anchor), or the underlying `opendir` error.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/flist.c:2028-2059` `secure_opendir()` - the confined open that
///   produces `scan_dirfd` for a daemon's directory scan.
/// - `rsync-3.5.0/syscall.c:136` `confinement_root()` - `am_daemon ? module_dir
///   : confine_root`, the value this parameter carries.
#[cfg(unix)]
pub fn read_dir_under(root: &Path, path: &Path) -> io::Result<ReadDir> {
    let Ok(relative) = path.strip_prefix(root) else {
        return read_dir(path);
    };
    // `strip_prefix` yields an empty path when `path` IS the root; the walk
    // spells that directory `.`, matching what upstream's post-`change_dir`
    // code uses for the same directory (`flist.c:2059`).
    let relative = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    let names = crate::confined_readdir::read_dir_confined(root, relative)?;
    Ok(ReadDir(Source::Names {
        dir: path.to_path_buf(),
        names: names.into_iter(),
    }))
}

/// Open `path` as an `O_PATH` descriptor, anchored on the pinned root when it
/// applies.
///
/// This is what a Landlock rule needs: the kernel wants the directory the rule
/// covers as a descriptor, and building that descriptor by re-opening the
/// absolute module path runs *after* the privilege drop. Anchoring it on the
/// pin is the difference between the module being sandboxed and the sandbox
/// silently failing to install on exactly the layout it was added for.
///
/// # Errors
///
/// The underlying `open`/`openat` error.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn open_o_path(path: &Path) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsFd;
    match crate::confinement::pinned_root_relative(path) {
        Some((fd, relative)) => imp::open_o_path_at(fd.as_fd(), &relative),
        None => imp::open_o_path_at_cwd(path),
    }
}

/// Iterator over a directory's children as absolute paths.
///
/// Produced by [`read_dir`]; the variant it wraps depends on whether the
/// directory was reachable through the pinned root.
pub struct ReadDir(Source);

enum Source {
    Std(std::fs::ReadDir),
    #[cfg(unix)]
    Names {
        dir: PathBuf,
        names: std::vec::IntoIter<std::ffi::OsString>,
    },
}

impl Iterator for ReadDir {
    type Item = io::Result<PathBuf>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            Source::Std(entries) => entries.next().map(|entry| entry.map(|e| e.path())),
            #[cfg(unix)]
            Source::Names { dir, names } => names.next().map(|name| Ok(dir.join(name))),
        }
    }
}

/// `Some(result)` when the lookup was anchored, `None` when the caller should
/// issue the ordinary path-based call.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn anchored_metadata(path: &Path, follow: bool) -> Option<io::Result<Metadata>> {
    use std::os::fd::AsFd;
    let (fd, relative) = crate::confinement::pinned_root_relative(path)?;
    Some(imp::metadata_at(fd.as_fd(), &relative, follow))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn anchored_metadata(_path: &Path, _follow: bool) -> Option<io::Result<Metadata>> {
    // No `O_PATH`: see the module docs. The caller takes the path-based arm.
    None
}

#[cfg(unix)]
mod imp {
    use super::*;

    use std::ffi::CString;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    use std::fs::File;
    use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    /// `stat`/`lstat` `relative` beneath `dirfd`.
    ///
    /// `O_PATH` is what makes this a stat and not a read: the kernel resolves
    /// the name and hands back a descriptor that refers to it without
    /// granting - or requiring - any access to its contents, so an unreadable
    /// file, a FIFO with no writer, and a device all report their metadata the
    /// way `lstat` does. `fstat` on such a descriptor has been supported since
    /// Linux 3.6. With `O_NOFOLLOW` it names the symlink itself, which is
    /// exactly `lstat`; without it the symlink is followed, which is `stat`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(super) fn metadata_at(
        dirfd: BorrowedFd<'_>,
        relative: &Path,
        follow: bool,
    ) -> io::Result<Metadata> {
        let mut flags = libc::O_PATH | libc::O_CLOEXEC;
        if !follow {
            flags |= libc::O_NOFOLLOW;
        }
        let fd = openat(dirfd, relative, flags)?;
        File::from(fd).metadata()
    }

    /// Open `relative` beneath `dirfd` as a directory for enumeration.
    pub(super) fn open_dir_at(dirfd: BorrowedFd<'_>, relative: &Path) -> io::Result<OwnedFd> {
        openat(
            dirfd,
            relative,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    }

    /// Open `relative` beneath `dirfd` as an `O_PATH` descriptor.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(super) fn open_o_path_at(dirfd: BorrowedFd<'_>, relative: &Path) -> io::Result<OwnedFd> {
        openat(dirfd, relative, libc::O_PATH | libc::O_CLOEXEC)
    }

    /// Open `path` as an `O_PATH` descriptor with no anchor.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(super) fn open_o_path_at_cwd(path: &Path) -> io::Result<OwnedFd> {
        // `AT_FDCWD` is the documented "resolve from the process cwd"
        // sentinel, not a descriptor, and an absolute path ignores it
        // entirely. Wrapping it as a `BorrowedFd` would misstate ownership, so
        // the raw form is passed straight through and this arm does not go
        // through `openat` above.
        let c_path = c_path(path)?;
        // SAFETY: `c_path` is a valid NUL-terminated C string that outlives
        // the call. `openat` is a thread-safe syscall wrapper returning either
        // a fresh owned descriptor or -1 with `errno` set; ownership of any
        // non-negative fd transfers immediately to `OwnedFd::from_raw_fd`,
        // which is its only owner and closes it on drop.
        #[allow(unsafe_code)]
        let raw = unsafe {
            libc::openat(
                libc::AT_FDCWD,
                c_path.as_ptr(),
                libc::O_PATH | libc::O_CLOEXEC,
            )
        };
        owned_or_errno(raw)
    }

    fn openat(dirfd: BorrowedFd<'_>, relative: &Path, flags: i32) -> io::Result<OwnedFd> {
        use std::os::fd::AsRawFd;

        let c_path = c_path(relative)?;
        // SAFETY: `dirfd` is a live borrowed descriptor for the duration of
        // the call and `c_path` is a valid NUL-terminated C string that
        // outlives it. `openat` is a thread-safe syscall wrapper returning
        // either a fresh owned descriptor or -1 with `errno` set; ownership of
        // any non-negative fd transfers immediately to `OwnedFd::from_raw_fd`,
        // which is its only owner and closes it on drop.
        #[allow(unsafe_code)]
        let raw = unsafe { libc::openat(dirfd.as_raw_fd(), c_path.as_ptr(), flags) };
        owned_or_errno(raw)
    }

    fn owned_or_errno(raw: i32) -> io::Result<OwnedFd> {
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a non-negative descriptor just returned by
        // `openat(2)` with `O_CLOEXEC`, not duplicated or retained anywhere
        // else, so this is its sole owner.
        #[allow(unsafe_code)]
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(fd)
    }

    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains interior null byte",
            )
        })
    }
}
