//! read_dir SEC-1.q2 cutover for the receiver-side `--delete` scan.
//!
//! [`read_dir_via_sandbox_or_fallback`] lists a directory either through
//! a sandbox-anchored `openat(O_DIRECTORY | O_NOFOLLOW)` + `fdopendir`
//! pass (materialised up front so the dirfd is released before per-entry
//! actions fire) or the path-based [`std::fs::read_dir`] fallback.
//! [`EntryKind`] / [`DirEntryView`] give the caller the directory /
//! symlink / other discriminator the delete loop needs, backfilling
//! [`libc::DT_UNKNOWN`] entries via [`fstatat_nofollow`].

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use super::errno_location;
use super::lstat::single_component_leaf;
use super::metadata::{fstatat_nofollow, widen_mode};
use super::open::{openat, openat_dot};

/// File-type discriminator returned by [`DirEntryView::file_type`].
///
/// The receiver-side `--delete` loop only needs to distinguish
/// directories, symlinks, and everything else (regular files, devices,
/// FIFOs, sockets); the kernel `d_type` field exposes the same shape
/// when available and [`fstatat_nofollow`] backfills it when the
/// underlying filesystem reports [`libc::DT_UNKNOWN`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// Directory entry.
    Dir,
    /// Symbolic link (never followed by the sandbox helpers).
    Symlink,
    /// Regular file, device, FIFO, socket, or unknown non-directory.
    Other,
}

impl EntryKind {
    fn from_dt(dt: u8) -> Option<Self> {
        match dt {
            libc::DT_DIR => Some(Self::Dir),
            libc::DT_LNK => Some(Self::Symlink),
            libc::DT_UNKNOWN => None,
            _ => Some(Self::Other),
        }
    }

    fn from_mode(mode: u32) -> Self {
        // `libc::S_IFMT` is `mode_t`: `u16` on macOS, `u32` on Linux.
        // [`widen_mode`] keeps the comparison portable without tripping
        // `clippy::unnecessary_cast` on either target.
        let masked = mode & widen_mode(libc::S_IFMT);
        if masked == widen_mode(libc::S_IFDIR) {
            Self::Dir
        } else if masked == widen_mode(libc::S_IFLNK) {
            Self::Symlink
        } else {
            Self::Other
        }
    }

    /// Returns `true` when the entry is a directory.
    #[must_use]
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Dir)
    }

    /// Returns `true` when the entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// One entry from [`ReadDirOutcome`] exposing the leaf name and the
/// classify bits the receiver-side `--delete` loop needs.
///
/// The view is produced by both the sandbox-anchored and the path-based
/// branches so the caller can swap on [`ReadDirOutcome`] without
/// branching on the underlying syscall family. The leaf name is owned so
/// the caller can hold it across further `*at` calls without keeping the
/// directory cursor live.
#[derive(Clone, Debug)]
pub struct DirEntryView {
    name: std::ffi::OsString,
    kind: Option<EntryKind>,
}

impl DirEntryView {
    /// The leaf name of this directory entry.
    #[must_use]
    pub fn file_name(&self) -> &std::ffi::OsStr {
        &self.name
    }

    /// Consume the view and return the owned leaf name.
    #[must_use]
    pub fn into_file_name(self) -> std::ffi::OsString {
        self.name
    }

    /// Classifies the entry without following symlinks, or `None` when
    /// the underlying filesystem reported [`libc::DT_UNKNOWN`] and the
    /// caller chose not to stat the leaf.
    #[must_use]
    pub fn file_type(&self) -> Option<EntryKind> {
        self.kind
    }
}

/// Result of [`read_dir_via_sandbox_or_fallback`].
///
/// The variant indicates which read path satisfied the call. Both
/// variants iterate as `io::Result<DirEntryView>` so the caller can swap
/// on the variant without branching on the per-entry shape.
#[derive(Debug)]
pub enum ReadDirOutcome {
    /// Sandbox-anchored listing collected via `openat(O_DIRECTORY |
    /// O_NOFOLLOW)` + `fdopendir`. The whole listing is materialised up
    /// front so the dirfd is released before the caller starts issuing
    /// per-entry actions (matches the `recursive_unlinkat` invariant).
    At(std::vec::IntoIter<DirEntryView>),
    /// Path-based [`std::fs::read_dir`] iterator used when the sandbox
    /// was unavailable or the relative path was not a single component.
    Std(std::fs::ReadDir),
}

impl Iterator for ReadDirOutcome {
    type Item = io::Result<DirEntryView>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::At(iter) => iter.next().map(Ok),
            Self::Std(iter) => iter.next().map(|res| {
                res.map(|entry| {
                    let kind = entry.file_type().ok().map(|ft| {
                        if ft.is_dir() {
                            EntryKind::Dir
                        } else if ft.is_symlink() {
                            EntryKind::Symlink
                        } else {
                            EntryKind::Other
                        }
                    });
                    DirEntryView {
                        name: entry.file_name(),
                        kind,
                    }
                })
            }),
        }
    }
}

/// Open `target_path` as a directory and list its entries, anchoring on
/// the sandbox dirfd when possible.
///
/// SEC-1.q2 adaptor for the receiver-side `--delete` scan (audit row #5).
/// Mirrors the existing `*_via_sandbox_or_fallback` shape:
/// - When `sandbox` is `Some` and `relative_path` is empty or `.`, the
///   listing targets `dest_dir` itself; the helper opens a fresh
///   `openat(dirfd, ".", O_DIRECTORY | O_RDONLY | O_CLOEXEC)` against
///   the sandbox dirfd so `fdopendir(3)` receives its own owned fd and
///   the caller's sandbox handle stays intact.
/// - When `sandbox` is `Some`, `target_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   `Component::Normal`, the helper opens the leaf through
///   `openat(sandbox.current_dirfd(), leaf, O_DIRECTORY | O_NOFOLLOW |
///   O_RDONLY | O_CLOEXEC)` so a TOCTOU symlink swap on the leaf cannot
///   redirect the listing into an attacker-chosen directory.
/// - In every other case the helper falls back to
///   [`std::fs::read_dir`] on `target_path`. The fallback is vulnerable
///   to the symlink-swap class the carrier closes; it is intended only
///   for the no-sandbox contexts and multi-component descents that the
///   SEC-1.f-q chain has not yet plumbed.
///
/// The sandbox-anchored branch materialises the full listing up front
/// (matching the `recursive_unlinkat` invariant) so the dirfd is closed
/// before per-entry `unlinkat`/`fstatat` syscalls fire. The std branch
/// lazily iterates as today.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ELOOP` or `ENOTDIR` when the leaf is a symlink and the sandbox
///   path was selected (`O_NOFOLLOW`).
/// - `ENOENT` when `target_path` does not exist.
/// - `EACCES` when the caller lacks search permission on the leaf or
///   the parent dirfd.
pub fn read_dir_via_sandbox_or_fallback(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    target_path: &Path,
) -> io::Result<ReadDirOutcome> {
    if let Some(sandbox) = sandbox {
        if relative_path.as_os_str().is_empty() || relative_path == Path::new(".") {
            if dest_dir == target_path {
                let parent = sandbox.current_dirfd();
                let dir_handle = openat_dot(parent)?;
                let entries = read_dir_entry_views(dir_handle, parent, None)?;
                return Ok(ReadDirOutcome::At(entries.into_iter()));
            }
        } else if let Some(leaf) = single_component_leaf(dest_dir, relative_path, target_path) {
            let parent = sandbox.current_dirfd();
            let dir_handle = openat(
                parent,
                leaf,
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
                0,
            )?;
            let entries = read_dir_entry_views(dir_handle, parent, Some(leaf))?;
            return Ok(ReadDirOutcome::At(entries.into_iter()));
        }
    }
    std::fs::read_dir(target_path).map(ReadDirOutcome::Std)
}

/// Materialise the full listing for the directory `dirfile` refers to,
/// classifying each entry by `d_type` when the filesystem provides it
/// and falling back to [`fstatat_nofollow`] off `parent_dirfd` when it
/// reports [`libc::DT_UNKNOWN`].
///
/// Consumes `dirfile`: ownership of the underlying fd is transferred to
/// the `DIR*` via `fdopendir(3)` and released by `closedir(3)` before
/// this helper returns. `parent_dirfd` plus `leaf_in_parent` describe
/// where to reopen the directory when a DT_UNKNOWN backfill is needed;
/// `leaf_in_parent == None` means `parent_dirfd` itself is the listing
/// target (the `dest_dir == target_path` branch).
fn read_dir_entry_views(
    dirfile: File,
    parent_dirfd: BorrowedFd<'_>,
    leaf_in_parent: Option<&OsStr>,
) -> io::Result<Vec<DirEntryView>> {
    use std::ffi::OsString;
    use std::os::fd::{FromRawFd, IntoRawFd};

    // SAFETY:
    // - `dirfile.into_raw_fd()` releases ownership of the raw fd to us;
    //   we hand that ownership directly to `fdopendir(3)`. On success
    //   the resulting `DIR*` owns the fd and `closedir(3)` will close
    //   it. On failure we reclaim ownership with `OwnedFd::from_raw_fd`
    //   so the standard `Drop` impl closes it exactly once.
    // - `dirfile` is not used after `into_raw_fd`, so the fd cannot be
    //   double-closed by `File::drop`.
    #[allow(unsafe_code)]
    let dirp = unsafe {
        let raw = dirfile.into_raw_fd();
        let ptr = libc::fdopendir(raw);
        if ptr.is_null() {
            let err = io::Error::last_os_error();
            let _reclaim = std::os::fd::OwnedFd::from_raw_fd(raw);
            return Err(err);
        }
        ptr
    };

    let mut entries: Vec<DirEntryView> = Vec::new();
    let mut needs_backfill = false;
    let result: io::Result<()> = loop {
        // SAFETY:
        // - `errno` is reset before every call so we can distinguish
        //   end-of-stream (`readdir` returns NULL with errno unchanged)
        //   from an error (`readdir` returns NULL with errno set).
        // - `dirp` is the live `DIR*` we just created; we hold it for
        //   the duration of the loop and `closedir` is called below.
        // - The returned `*mut dirent` is owned by the C runtime and is
        //   only valid until the next `readdir(3)` call on the same
        //   `DIR*`; we copy `d_name` into an owned `OsString` before
        //   the next iteration so the borrow does not outlive the
        //   pointer.
        #[allow(unsafe_code)]
        let ent_ptr = unsafe {
            *errno_location() = 0;
            libc::readdir(dirp)
        };
        if ent_ptr.is_null() {
            // SAFETY: `errno_location` returns a thread-local
            // `*mut c_int` whose lifetime is the calling thread.
            #[allow(unsafe_code)]
            let raw_errno = unsafe { *errno_location() };
            if raw_errno == 0 {
                break Ok(());
            }
            break Err(io::Error::from_raw_os_error(raw_errno));
        }
        // SAFETY: `ent_ptr` is non-NULL per the check above; the
        // pointed-to `dirent` is owned by the C runtime for the
        // lifetime of this `readdir` call. We read `d_name` and
        // `d_type` and copy `d_name` out before issuing the next
        // `readdir`.
        #[allow(unsafe_code)]
        let (name_bytes, dt) = unsafe {
            let name_ptr = (*ent_ptr).d_name.as_ptr();
            let cstr = std::ffi::CStr::from_ptr(name_ptr);
            let dt = (*ent_ptr).d_type;
            (cstr.to_bytes().to_vec(), dt)
        };
        let name = OsString::from_vec(name_bytes);
        if name.as_bytes() == b"." || name.as_bytes() == b".." {
            continue;
        }
        let kind = EntryKind::from_dt(dt);
        if kind.is_none() {
            needs_backfill = true;
        }
        entries.push(DirEntryView { name, kind });
    };

    // SAFETY: `dirp` is the live `DIR*` we created above; `closedir(3)`
    // closes the underlying fd and frees the C-runtime state. After
    // this call `dirp` must not be dereferenced.
    #[allow(unsafe_code)]
    unsafe {
        libc::closedir(dirp);
    }

    result?;

    // Backfill DT_UNKNOWN entries with `fstatat` against a freshly
    // opened dirfd. Filesystems such as XFS and many FUSE mounts always
    // report DT_UNKNOWN; without the backfill the receiver cannot
    // distinguish a directory from a regular file and would mis-dispatch
    // the unlink. The reopen anchors on the same parent that produced
    // the listing so the stat is consistent with the directory we just
    // read.
    if needs_backfill {
        let backfill_dir = match leaf_in_parent {
            Some(leaf) => openat(
                parent_dirfd,
                leaf,
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
                0,
            )?,
            None => openat_dot(parent_dirfd)?,
        };
        let backfill_fd = std::os::fd::AsFd::as_fd(&backfill_dir);
        for entry in entries.iter_mut().filter(|e| e.kind.is_none()) {
            entry.kind = match fstatat_nofollow(backfill_fd, &entry.name) {
                Ok(meta) => Some(EntryKind::from_mode(meta.mode())),
                Err(err) if err.raw_os_error() == Some(libc::ENOENT) => None,
                Err(err) => return Err(err),
            };
        }
    }

    Ok(entries)
}
