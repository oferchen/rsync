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
//! - `rsync-3.5.2/main.c:865` - `get_local_name()` enters the destination.
//! - `rsync-3.5.2/util1.c:1448-1466` - `change_dir()` `fchdir()`s to the walked
//!   descriptor.
//! - `rsync-3.5.2/receiver.c:413` - `open_tmpfile()` creates the temp file by
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
    use std::os::fd::AsRawFd;
    match crate::confinement::pinned_destination_relative(path) {
        Some((fd, relative)) => openat_name(fd.as_raw_fd(), &relative, flags, mode),
        None => openat_name(libc::AT_FDCWD, path, flags, mode),
    }
}

/// `openat(dirfd, name, flags | O_CLOEXEC, mode)`.
#[cfg(unix)]
fn openat_name(
    dirfd: libc::c_int,
    name: &Path,
    flags: libc::c_int,
    mode: u32,
) -> io::Result<std::os::fd::OwnedFd> {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let c_name = CString::new(name.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: the caller keeps `dirfd` - `AT_FDCWD` or a descriptor it holds -
    // open for the duration of the call; `c_name` is a valid NUL-terminated
    // string. `openat` returns a new descriptor or -1.
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

/// The directory holding `path`'s leaf, resolved beneath the pinned
/// destination the way upstream's secure walk resolves a relative name below
/// its cwd, and the leaf name.
///
/// Each directory component is opened `O_NOFOLLOW | O_DIRECTORY`; a symlink in
/// its place is followed only while its target is relative and stays beneath
/// the pin, whoever owns it. An absolute or empty target, a `..` above the pin
/// and an exhausted hop budget are refused with `ELOOP`. `None` means nothing
/// is pinned or `path` is not beneath the pin, and the caller resolves `path`
/// as it always has.
///
/// # Errors
///
/// The walk's refusal or the underlying `openat`/`readlinkat` error;
/// `EISDIR` when `path` is the pinned destination itself.
///
/// # Upstream Reference
///
/// - `rsync-3.5.2/syscall.c:3103-3177` `ds_descend()` - the per-component rule.
/// - `rsync-3.5.2/syscall.c:3196-3311` `secure_walk_at()`.
#[cfg(unix)]
fn secure_parent(path: &Path) -> io::Result<Option<(std::os::fd::OwnedFd, std::ffi::OsString)>> {
    use std::os::fd::AsFd;
    let Some((pin, relative)) = crate::confinement::pinned_destination_relative(path) else {
        return Ok(None);
    };
    let Some(leaf) = relative.file_name() else {
        return Err(io::Error::from_raw_os_error(libc::EISDIR));
    };
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let dir = crate::dir_sandbox::walk_beneath(pin.as_fd(), parent)?;
    Ok(Some((dir, leaf.to_os_string())))
}

/// `lstat` for a destination entry whose directory components a peer can
/// influence, resolved through the secure walk beneath the pinned
/// destination (see `secure_parent`). Unpinned, it is [`symlink_metadata`].
///
/// upstream: `rsync-3.5.2/generator.c:2214-2216` checks `partialptr` with
/// `link_stat()`, an `x_lstat()` (`flist.c:519`) that goes through
/// `do_lstat_at()` (`syscall.c:2471`) and so resolves the relative partial-dir
/// parent with the secure walk; a refusal leaves `partialptr` NULL.
///
/// # Errors
///
/// The walk's refusal (`ELOOP`) or the underlying `fstatat`/`lstat` error.
pub fn symlink_metadata_secure(path: &Path) -> io::Result<EntryMetadata> {
    #[cfg(unix)]
    if let Some((dir, leaf)) = secure_parent(path)? {
        use std::os::fd::AsFd;
        return crate::dir_sandbox::fstatat_nofollow(dir.as_fd(), &leaf).map(EntryMetadata::At);
    }
    symlink_metadata(path)
}

/// `unlink(2)` of a destination entry whose directory components a peer can
/// influence, through the secure walk beneath the pinned destination (see
/// `secure_parent`); unpinned, it is [`std::fs::remove_file`]. A partial dir
/// that is a symlink out of the destination is refused instead of having a
/// file outside the tree removed.
///
/// upstream: `rsync-3.5.2/receiver.c:1307-1314` removes a consumed
/// `--partial-dir` basis with `do_unlink_at()` under `operator_path_resolve`,
/// never by a path that follows a symlinked partial dir out of the tree.
///
/// # Errors
///
/// The walk's refusal (`ELOOP`) or the underlying `unlinkat`/`unlink` error.
pub fn remove_file_secure(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some((dir, leaf)) = secure_parent(path)? {
        use std::os::fd::AsFd;
        return crate::dir_sandbox::at_syscalls::unlinkat(
            dir.as_fd(),
            &leaf,
            crate::dir_sandbox::UnlinkFlags::File,
        );
    }
    std::fs::remove_file(path)
}

/// `open(2)` of a destination entry whose directory components a peer can
/// influence: the parent through the secure walk beneath the pinned
/// destination (see `secure_parent`), the leaf with `flags | O_NOFOLLOW`.
/// `None` when nothing is pinned or `path` is not beneath the pin.
///
/// upstream: `rsync-3.5.2/receiver.c:1083-1087` opens a relative
/// `--partial-dir` basis with `secure_relative_open(NULL, fnamecmp,
/// O_RDONLY, 0)` (`syscall.c:3314`).
///
/// # Errors
///
/// The walk's refusal (`ELOOP`), `ELOOP` for a symlink leaf, or the
/// underlying `openat` error.
#[cfg(unix)]
pub fn open_secure(path: &Path, flags: libc::c_int) -> io::Result<Option<std::os::fd::OwnedFd>> {
    use std::os::fd::AsRawFd;
    let Some((dir, leaf)) = secure_parent(path)? else {
        return Ok(None);
    };
    openat_name(
        dir.as_raw_fd(),
        Path::new(&leaf),
        flags | libc::O_NOFOLLOW,
        0,
    )
    .map(Some)
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

/// Opens a destination file for reading, anchored on the pinned destination.
///
/// Upstream reads a basis, a `--partial-dir` leaf, a `--fuzzy` candidate and
/// the `--inplace --backup` pre-image by a name relative to the cwd
/// `change_dir()` entered, so a destination swapped after the walk cannot
/// redirect the read to another directory's file. A symlink leaf is refused,
/// as upstream opens both with `O_NOFOLLOW`.
///
/// upstream: `rsync-3.5.2/receiver.c:1086-1093` opens the basis by its
/// relative name (`do_open_atfd()` adds `O_NOFOLLOW`, syscall.c:4084);
/// `rsync-3.5.2/util1.c:411-412` `copy_file()` opens the pre-image the same
/// way.
///
/// # Errors
///
/// The underlying `openat`/`open` error; `ELOOP` for a symlink leaf.
pub fn open_read(path: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        open(path, libc::O_RDONLY | libc::O_NOFOLLOW, 0).map(std::fs::File::from)
    }
    #[cfg(not(unix))]
    {
        std::fs::File::open(path)
    }
}

/// Lists a destination directory, anchored on the pinned destination, with
/// each entry's `lstat`.
///
/// `.` and `..` are omitted and the order is the directory's own. An entry
/// whose `lstat` fails - removed between the listing and the stat - is left
/// out, as a vanished candidate is of no use to the caller.
///
/// upstream: `rsync-3.5.2/generator.c:2230` reads the `--fuzzy` dirlist by a
/// name relative to the entered destination (`get_dirlist(dn, -1, ...)`), so a
/// swapped destination cannot offer another directory's files as candidates.
///
/// # Errors
///
/// The error opening or reading the directory.
pub fn read_dir(path: &Path) -> io::Result<Vec<(std::ffi::OsString, EntryMetadata)>> {
    #[cfg(unix)]
    if crate::confinement::pinned_destination_relative(path).is_some() {
        use std::os::fd::AsFd;
        let dir = std::fs::File::from(open_dir(path)?);
        let lookup = dir.try_clone()?;
        let names = crate::dir_sandbox::at_syscalls::list_dir_fd(dir)?;
        return Ok(names
            .into_iter()
            .filter_map(|entry| {
                let name = entry.into_file_name();
                crate::dir_sandbox::fstatat_nofollow(lookup.as_fd(), &name)
                    .ok()
                    .map(|meta| (name, EntryMetadata::At(meta)))
            })
            .collect());
    }
    Ok(std::fs::read_dir(path)?
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some((entry.file_name(), EntryMetadata::Std(meta)))
        })
        .collect())
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
/// [`crate::set_path_times`] follows it.
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
        crate::set_path_times(path, None, Some(mtime), true)
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
/// `call`, so the number cannot be reused under it. When nothing is pinned, or
/// `path` lies outside the pin, `call` gets `Some(path)` itself.
///
/// A pinned path that has no such spelling - `/proc` not mounted (a chroot
/// without it), or a Unix without `/proc/self/fd` - gives `call` `None`: the
/// consumer then reaches the entry through a descriptor opened beneath the pin
/// ([`open`], [`open_dir`]), or by the means its platform offers, rather than
/// resolving `path` from `/` as if nothing were pinned.
pub fn with_anchored_path<T>(path: &Path, call: impl FnOnce(Option<&Path>) -> T) -> T {
    #[cfg(unix)]
    if let Some((fd, relative)) = crate::confinement::pinned_destination_relative(path) {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            static PROC_FD_MOUNTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *PROC_FD_MOUNTED.get_or_init(|| Path::new("/proc/self/fd").is_dir()) {
                let anchored =
                    std::path::PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                        .join(relative);
                let result = call(Some(&anchored));
                drop(fd);
                return result;
            }
        }
        drop((fd, relative));
        return call(None);
    }
    call(Some(path))
}
