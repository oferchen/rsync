//! Destination-side filesystem calls anchored on the pinned destination root.
//!
//! Upstream's receiver enters the destination once - `get_local_name()` calls
//! `change_dir()`, which `fchdir()`s to the descriptor the ownership walk
//! returned - and names every later entry relative to that cwd. A destination
//! renamed or swapped for a symlink mid-transfer therefore cannot redirect a
//! later temp-file create, stat or attribute change: they all resolve from the
//! directory that was entered.
//!
//! oc keeps absolute destination paths and never `chdir`s. Each helper here
//! asks [`crate::confinement::pinned_destination_relative`] whether the path
//! lies beneath the pinned destination; if it does the call is issued
//! `*at`-style on the held descriptor, which is the inode upstream's cwd
//! names, and if it does not - nothing pinned, or a path elsewhere such as an
//! absolute `--temp-dir` - it is the ordinary path-based call. Both arms make
//! the same single syscall, so anchoring costs no extra round trip.
//!
//! Every component below the pin resolves exactly as upstream's relative name
//! would after `fchdir()`: no `AT_`/`RESOLVE_` flag is added, so this changes
//! where a lookup starts, never what it is allowed to follow.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/main.c:865` - `get_local_name()` enters the destination.
//! - `rsync-3.5.1/util1.c:1448-1466` - `change_dir()` `fchdir()`s to the walked
//!   descriptor.
//! - `rsync-3.5.1/receiver.c:413` - `open_tmpfile()` creates the temp file by
//!   a name relative to that cwd.

use std::io;
use std::path::Path;

use crate::EntryMetadata;

/// `lstat` for a destination path, anchored on the pinned destination.
///
/// # Errors
///
/// The underlying `fstatat`/`lstat` error.
pub fn symlink_metadata(path: &Path) -> io::Result<EntryMetadata> {
    #[cfg(unix)]
    if let Some((fd, relative)) = crate::confinement::pinned_destination_relative(path) {
        use std::os::fd::AsFd;
        return crate::dir_sandbox::fstatat_nofollow(fd.as_fd(), relative.as_os_str())
            .map(EntryMetadata::At);
    }
    std::fs::symlink_metadata(path).map(EntryMetadata::Std)
}

/// `stat` for a destination path, anchored on the pinned destination.
///
/// # Errors
///
/// The underlying `fstatat`/`stat` error.
pub fn metadata(path: &Path) -> io::Result<EntryMetadata> {
    #[cfg(unix)]
    if let Some((fd, relative)) = crate::confinement::pinned_destination_relative(path) {
        use std::os::fd::AsFd;
        return crate::dir_sandbox::fstatat_follow(fd.as_fd(), relative.as_os_str())
            .map(EntryMetadata::At);
    }
    std::fs::metadata(path).map(EntryMetadata::Std)
}

/// `open(2)` of a destination path with `flags` and `mode`, anchored on the
/// pinned destination. `O_CLOEXEC` is always added.
///
/// # Errors
///
/// The underlying `openat` error, or `EINVAL` for a path with an interior NUL.
#[cfg(unix)]
pub fn open(path: &Path, flags: libc::c_int, mode: u32) -> io::Result<std::os::fd::OwnedFd> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let anchored = crate::confinement::pinned_destination_relative(path);
    let (dirfd, name) = match &anchored {
        Some((fd, relative)) => (fd.as_raw_fd(), relative.as_path()),
        None => (libc::AT_FDCWD, path),
    };
    let c_name = CString::new(name.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `dirfd` is either `AT_FDCWD` or the raw fd of the pinned
    // destination, which `anchored` keeps alive for the duration of the call;
    // `c_name` is a valid NUL-terminated string. `openat` returns a new
    // descriptor or -1.
    let raw = unsafe {
        libc::openat(
            dirfd,
            c_name.as_ptr(),
            flags | libc::O_CLOEXEC,
            libc::c_uint::from(mode),
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a freshly opened descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Creates a new destination file for writing, failing if the name exists -
/// `OpenOptions::new().write(true).create_new(true)` anchored on the pin.
///
/// # Errors
///
/// As [`open`]; `EEXIST` when the name is already taken.
#[cfg(unix)]
pub fn create_new(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    open(path, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, mode).map(std::fs::File::from)
}

/// Opens a destination directory for reading, anchored on the pin.
///
/// # Errors
///
/// As [`open`]; `ENOTDIR` when `path` is not a directory.
#[cfg(unix)]
pub fn open_dir(path: &Path) -> io::Result<std::os::fd::OwnedFd> {
    open(path, libc::O_RDONLY | libc::O_DIRECTORY, 0)
}

/// `chmod(2)` of a destination path, anchored on the pinned destination. The
/// leaf is followed, as `chmod` follows it.
///
/// # Errors
///
/// The underlying `fchmodat`/`chmod` error.
pub fn set_permissions(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        at_call(path, |dirfd, name| {
            // SAFETY: `name` is a valid NUL-terminated string and `dirfd` is
            // `AT_FDCWD` or the pinned descriptor kept alive by the caller.
            unsafe { libc::fchmodat(dirfd, name, mode as libc::mode_t, 0) }
        })
    }
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(mode & 0o200 == 0);
        std::fs::set_permissions(path, permissions)
    }
}

/// Sets a destination path's modification time, leaving its access time,
/// anchored on the pinned destination. The leaf is followed, as
/// `filetime::set_file_mtime` follows it.
///
/// # Errors
///
/// The underlying `utimensat` error.
pub fn set_mtime(path: &Path, mtime: filetime::FileTime) -> io::Result<()> {
    #[cfg(unix)]
    {
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            },
            libc::timespec {
                tv_sec: mtime.unix_seconds() as _,
                tv_nsec: libc::c_long::from(mtime.nanoseconds() as i32),
            },
        ];
        at_call(path, |dirfd, name| {
            // SAFETY: `name` is a valid NUL-terminated string, `dirfd` is
            // `AT_FDCWD` or the pinned descriptor kept alive by the caller, and
            // `times` is a two-element array as `utimensat` requires.
            unsafe { libc::utimensat(dirfd, name, times.as_ptr(), 0) }
        })
    }
    #[cfg(not(unix))]
    {
        filetime::set_file_mtime(path, mtime)
    }
}

/// Runs one `*at` call for `path`: on the pinned destination descriptor with
/// the remainder beneath it, or on `AT_FDCWD` with the path itself.
#[cfg(unix)]
fn at_call(
    path: &Path,
    call: impl FnOnce(libc::c_int, *const libc::c_char) -> libc::c_int,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let anchored = crate::confinement::pinned_destination_relative(path);
    let (dirfd, name) = match &anchored {
        Some((fd, relative)) => (fd.as_raw_fd(), relative.as_path()),
        None => (libc::AT_FDCWD, path),
    };
    let c_name = CString::new(name.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    if call(dirfd, c_name.as_ptr()) == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Runs `call` with a spelling of `path` that resolves through the pinned
/// destination descriptor, for consumers that accept only a path - libacl's
/// `acl_get_file()` behind the default-ACL read.
///
/// On Linux that spelling is the descriptor's `/proc/self/fd/N` magic link
/// joined with the remainder, which the kernel resolves exactly as
/// `fstatat(N, remainder)` would, so the consumer's own syscalls are anchored
/// without changing in number. The descriptor is held for the duration of
/// `call`, so the number cannot be reused under it. Elsewhere, when nothing is
/// pinned, or when `/proc` is not mounted (a chroot without it), `call` gets
/// `path` itself.
pub fn with_anchored_path<T>(path: &Path, call: impl FnOnce(&Path) -> T) -> T {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        static PROC_FD_MOUNTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if let Some((fd, relative)) = crate::confinement::pinned_destination_relative(path)
            && *PROC_FD_MOUNTED.get_or_init(|| Path::new("/proc/self/fd").is_dir())
        {
            let anchored = std::path::PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                .join(relative);
            let result = call(&anchored);
            drop(fd);
            return result;
        }
    }
    call(path)
}
