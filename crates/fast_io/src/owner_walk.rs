//! Ownership-trusted resolution for **operator-supplied** paths.
//!
//! An operator path - a `--backup-dir`, `--temp-dir`, `--partial-dir` or
//! alt-dest the person running rsync named - may legitimately point outside the
//! transfer tree, so the confined walk in [`crate::dir_sandbox`] is the wrong
//! policy for it: location cannot be the trust signal. Upstream's answer is to
//! make **authority** the signal instead - follow a symlink owned by uid 0 or
//! our own euid (the operator's own layout), refuse any other-uid one, at every
//! component.
//!
//! That distinction is the whole defence in upstream's
//! `backup-dir-symlink-race` test: an attacker who can create entries inside the
//! backup tree flips a parent component between a real directory and a symlink
//! pointing outside, and a path-based rename lands the backup wherever the
//! symlink pointed at the instant the kernel resolved it.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:558` `owner_walk_parent()` - open the parent of an
//!   operator path via the ownership walk, hand back the final component.
//! - `rsync-3.5.0/syscall.c:286` `ona_open()` - the per-component walk itself.
//! - `rsync-3.5.0/syscall.c:406` - the ownership test:
//!   `if (lst.st_uid != 0 && lst.st_uid != trusted_uid)` refuse with `ELOOP`.
//! - `rsync-3.5.0/syscall.c:544-551` - "An operator path may legitimately point
//!   outside the tree, so the trust signal is authority (ownership), not
//!   location."

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

/// Symlink-follow budget for one walk, spent across every component.
///
/// upstream: `rsync-3.5.0/syscall.c:361` `int loops = 40;` - "SYMLOOP_MAX-ish;
/// breaks symlink cycles. Counts symlink expansions only, NOT path depth."
const MAX_SYMLINK_HOPS: u32 = 40;

/// Effective uid, the second trusted owner alongside root.
///
/// upstream: `rsync-3.5.0/syscall.c:304` `const uid_t trusted_uid = geteuid();`
fn trusted_uid() -> u32 {
    // SAFETY: `geteuid(2)` takes no arguments, cannot fail, and returns a plain
    // integer. It is one of the few POSIX calls with no error path at all.
    #[allow(unsafe_code)]
    unsafe {
        libc::geteuid()
    }
}

/// Open a directory component beneath `dirfd`, refusing a symlink at the leaf.
///
/// The caller has already `statat`'d the component and found it is not a
/// symlink; `O_NOFOLLOW` closes the window between that check and this open, so
/// a component flipped to a symlink in between fails rather than resolves.
fn open_dir_component(dirfd: BorrowedFd<'_>, name: &OsStr) -> io::Result<OwnedFd> {
    rustix::fs::openat(
        dirfd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))
}

/// Split `path` into the components of its parent plus the final name.
///
/// Returns `None` when the path has no final component (`/`, `.`, `..`, or
/// empty), which no operator path being renamed onto can have.
fn split_parent(path: &Path) -> Option<(Vec<OsString>, OsString)> {
    let leaf = path.file_name()?.to_os_string();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let mut names = Vec::new();
    for component in parent.components() {
        match component {
            Component::Normal(name) => names.push(name.to_os_string()),
            Component::ParentDir => names.push(OsString::from("..")),
            // `/` is expressed by the walk starting at the root; `.` is a no-op;
            // a Windows prefix cannot occur under `#![cfg(unix)]`.
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    Some((names, leaf))
}

/// Push the components of `path` onto the front of `pending`, in order.
fn prepend_components(pending: &mut Vec<OsString>, path: &Path) {
    let mut head: Vec<OsString> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            _ => None,
        })
        .collect();
    head.append(pending);
    *pending = head;
}

/// Open the parent directory of `path` via the ownership walk.
///
/// Every component is inspected with `AT_SYMLINK_NOFOLLOW` before it is opened.
/// A symlink owned by uid 0 or the euid is followed (its target is spliced into
/// the remaining path, an absolute target restarting the walk at `/`); a symlink
/// owned by anyone else is refused.
///
/// Returns the parent descriptor plus the final component, ready for a `*at`
/// operation.
///
/// # Errors
///
/// - `ELOOP` when a component is a symlink owned by an untrusted uid, or the
///   hop budget is exhausted. This is the security refusal, and it is
///   deliberately not `EXDEV`: callers treat `EXDEV` as cross-device and fall
///   back to copy+remove, which would defeat the refusal.
/// - `EINVAL` when `path` has no final component.
/// - Otherwise the `openat`/`statat`/`readlinkat` errno verbatim.
pub fn owner_trusted_parent(path: &Path) -> io::Result<(OwnedFd, OsString)> {
    let Some((mut pending, leaf)) = split_parent(path) else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };

    let start = if path.is_absolute() { "/" } else { "." };
    let mut dirfd = rustix::fs::open(
        start,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;

    let trusted = trusted_uid();
    let mut hops = MAX_SYMLINK_HOPS;

    while !pending.is_empty() {
        let name = pending.remove(0);
        let stat = rustix::fs::statat(dirfd.as_fd(), name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;

        if FileType::from_raw_mode(stat.st_mode as _) != FileType::Symlink {
            dirfd = open_dir_component(dirfd.as_fd(), name.as_os_str())?;
            continue;
        }

        // upstream: syscall.c:406 - an other-uid symlink is the attacker's, and
        // is refused; uid 0 or our own euid is the operator's own layout.
        if stat.st_uid != 0 && stat.st_uid != trusted {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        if hops == 0 {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        hops -= 1;

        let target = rustix::fs::readlinkat(dirfd.as_fd(), name.as_os_str(), Vec::new())
            .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;
        let target = PathBuf::from(OsStr::from_bytes(target.as_bytes()));
        if target.is_absolute() {
            // upstream: syscall.c:422 "Absolute target restarts the walk from /".
            dirfd = rustix::fs::open(
                "/",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;
        }
        prepend_components(&mut pending, &target);
    }

    Ok((dirfd, leaf))
}

/// Rename `old_path` to `new_path` with both endpoints resolved by the
/// ownership walk.
///
/// This is the operator-path counterpart to
/// [`confined_rename`](crate::confined_rename): that one confines beneath a
/// transfer root, this one trusts by ownership so an operator directory outside
/// the tree still works. Each side is walked independently, mirroring upstream.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:1894` `do_rename_at()` under `operator_path_resolve`
///   - `owner_walk_parent` on each side, then `renameat`.
/// - `rsync-3.5.0/backup.c:200-219` `make_backup()` - the caller that sets the
///   operator-path mode around the backup rename.
///
/// # Errors
///
/// Propagates the walk's refusal (`ELOOP`) or the `renameat(2)` errno.
pub fn operator_rename(old_path: &Path, new_path: &Path, replace: bool) -> io::Result<()> {
    let (old_dirfd, old_leaf) = owner_trusted_parent(old_path)?;
    let (new_dirfd, new_leaf) = owner_trusted_parent(new_path)?;
    crate::renameat(
        old_dirfd.as_fd(),
        &old_leaf,
        new_dirfd.as_fd(),
        &new_leaf,
        replace,
    )
}

/// Hard-link `old_path` to `new_path` with both endpoints resolved by the
/// ownership walk.
///
/// The link tier runs *before* the rename tier in a backup, so confining only
/// the rename would leave the escape wide open - upstream sets the
/// operator-path mode around `link_or_rename()` as a whole, covering both.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/backup.c:200-207` `link_or_rename()` - `do_link_at` first,
///   `do_rename_at` on failure.
/// - `rsync-3.5.0/syscall.c:676` `do_link_at()` under `operator_path_resolve` -
///   `owner_walk_parent` on each side, then `linkat`.
///
/// # Errors
///
/// Propagates the walk's refusal (`ELOOP`) or the `linkat(2)` errno - notably
/// `EXDEV` when the backup area is on another filesystem, which the caller
/// treats as its signal to fall through to the next tier.
pub fn operator_link(old_path: &Path, new_path: &Path) -> io::Result<()> {
    let (old_dirfd, old_leaf) = owner_trusted_parent(old_path)?;
    let (new_dirfd, new_leaf) = owner_trusted_parent(new_path)?;
    crate::linkat(old_dirfd.as_fd(), &old_leaf, new_dirfd.as_fd(), &new_leaf)
}
