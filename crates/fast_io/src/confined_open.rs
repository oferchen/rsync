//! Sender-side confined source open beneath a module root.
//!
//! [`open_source_confined`] opens a source file for reading such that the
//! kernel guarantees resolution cannot escape the module-root directory:
//! no `..` climb above the root, no absolute-path jump, no symlink whose
//! target lies outside the root, and no `/proc` magic-link trick. Symlinks
//! that stay *within* the module root are followed normally, matching the
//! non-chroot daemon behaviour upstream restored for issue #715.
//!
//! This is the sender-side analogue of the receiver's
//! [`crate::secure_dir::secure_open_dir`] / [`crate::dir_sandbox::DirSandbox`]
//! confinement. It exists to close the TOCTOU window where an attacker who
//! controls the module tree swaps a directory component for a symlink
//! between the file-list scan and the source open, redirecting the read to
//! a file outside the module.
//!
//! # The leaf is a separate decision from the walk
//!
//! Upstream splits the confined sender open into two entry points that share
//! the same confinement and differ only in how they treat the *final*
//! component, so [`LeafPolicy`] carries that choice as a parameter rather
//! than letting one arm silently answer for both:
//!
//! - `rsync-3.5.0/sender.c:209-247` `sender_open_confined()` - the default.
//!   Intermediate in-tree symlinks are followed, the leaf is opened
//!   `O_NOFOLLOW` so a raced leaf-symlink swap is refused.
//! - `rsync-3.5.0/sender.c:250-330` `sender_open_copylinks_confined()` -
//!   `--copy-links` / `--copy-unsafe-links` / `--copy-dirlinks`. The leaf
//!   symlink is resolved deliberately, still beneath the root.
//!
//! # Platform behaviour
//!
//! upstream: `sender.c:678-682`, which selects these helpers when
//! `secure_relpath_active()` (a non-chroot daemon, or the `/./` inner-module
//! chroot).
//!
//! - Linux 5.6+: `openat2(2)` with
//!   `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS` anchored at the module-root
//!   dirfd, plus `O_NOFOLLOW` under [`LeafPolicy::Nofollow`]. The kernel
//!   applies `O_NOFOLLOW` to the final component only, so intermediate
//!   in-tree symlinks are still followed - the same split upstream builds by
//!   hand. Escapes fail with `EXDEV`.
//! - Older Linux and every other Unix target: the shared per-component
//!   resolver ([`crate::dir_sandbox::DirSandbox::open_dest_anchor_confined`])
//!   walks the parent directories, mirroring upstream's `ds_descend()`, then
//!   the leaf is opened against the resolved parent dirfd.
//! - Non-Unix: unsupported. The oc daemon (the only caller) is Unix-only,
//!   so this path is never reached; it returns an `Unsupported` error.

use std::fs::File;
use std::io;
use std::path::{Component, Path};

/// How the final component of a confined source open is resolved.
///
/// The two variants are upstream's two entry points, not a tuning knob: the
/// caller already knows whether the operator asked for symlink following, and
/// making that an argument means neither platform arm can answer it by
/// accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafPolicy {
    /// Refuse a symlinked leaf with `ELOOP`, so a leaf raced into a symlink
    /// between the file-list scan and the open cannot redirect the read.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/sender.c:209-247` `sender_open_confined()`
    Nofollow,
    /// Resolve a symlinked leaf, still confined beneath the root: an absolute
    /// target is refused, a relative one is re-resolved through the walk.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/sender.c:250-330` `sender_open_copylinks_confined()`
    FollowConfined,
}

/// Opens `root`/`relative` for reading, confined beneath `root`.
///
/// `root` is the operator-trusted module-root directory (an absolute path
/// from the daemon config); it is opened with normal symlink resolution.
/// `relative` is the module-relative source path; its resolution is
/// confined so it cannot escape `root`. `leaf` selects the final-component
/// rule - see [`LeafPolicy`].
///
/// When `noatime` is set the leaf open adds `O_NOATIME` on Linux/Android,
/// falling back to a plain open if the kernel or filesystem rejects the
/// flag (`EPERM`/`EACCES`/`EINVAL`/`ENOTSUP`/`EROFS`), matching
/// [`crate::nofollow_open`] and the source-open helper.
///
/// # Errors
///
/// - `EINVAL` when `relative` is absolute or contains a `..` component
///   (front-door validation, mirroring upstream `secure_relative_open`).
/// - `EISDIR` when `relative` names a directory rather than a file: it is
///   empty, all slashes, or its final component is `.` or `..`.
/// - `EXDEV` (Linux `openat2` path) when resolution would escape `root`.
/// - `ELOOP` when a symlink is refused: a leaf symlink under
///   [`LeafPolicy::Nofollow`], an absolute or escaping target under
///   [`LeafPolicy::FollowConfined`], or an exhausted hop budget.
/// - `ENOENT` when the file does not exist beneath `root`.
/// - Any other I/O error from the underlying syscalls, forwarded verbatim.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/sender.c:678-682` `send_files()` - the call site that picks
///   between the two helpers this function's `leaf` argument names.
pub fn open_source_confined(
    root: &Path,
    relative: &Path,
    leaf: LeafPolicy,
    noatime: bool,
) -> io::Result<File> {
    validate_relative(relative)?;
    imp::open_source_confined(root, relative, leaf, noatime)
}

/// What the destination leaf is expected to be, so the pin can ask for
/// `O_DIRECTORY` where upstream does.
///
/// Upstream derives this from the *intended* mode rather than the on-disk
/// one, because an attacker controls the latter through the very flip the
/// pin exists to refuse.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/rsync.c:589-594` the `odir` computation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestLeafKind {
    /// A regular file or a FIFO. `O_NONBLOCK` is what keeps the FIFO open
    /// from blocking on a missing writer.
    NonDirectory,
    /// A directory: the open adds `O_DIRECTORY` so a leaf raced into a
    /// non-directory is refused rather than pinned.
    Directory,
}

/// Pins the destination leaf `root`/`relative` with a confined,
/// `O_NOFOLLOW` open so metadata writes can be driven off the returned fd
/// instead of re-resolving the path.
///
/// This is the destination-side counterpart to [`open_source_confined`]:
/// same walk, same refusals, but it opens the leaf purely to *hold* it. A
/// path-based `lsetxattr`/`lsetacl` re-walks the parent components on every
/// call, so a parent flipped to a symlink between the confined create and
/// the metadata write redirects an attacker-chosen xattr outside the
/// destination tree. An fd cannot be redirected that way.
///
/// The parent walk follows relative in-tree symlinks and refuses absolute
/// targets, `..` above the anchor, and an exhausted hop budget - the same
/// `ds_descend()` rule [`open_source_confined`] uses. `O_NOFOLLOW` then
/// governs the leaf alone.
///
/// A caller that gets an error here must **skip** the metadata write, not
/// fall back to the path-based variant: falling back reinstates exactly the
/// redirect this refuses. That is upstream's `xattr_refuse`.
///
/// # Errors
///
/// - `EINVAL` when `relative` is absolute or contains a `..` component.
/// - `EISDIR` when `relative` has no leaf to pin: it is empty, all slashes,
///   or its final component is `.` or `..`.
/// - `ELOOP` when a symlink is refused - the leaf itself, an absolute or
///   escaping target in the parent walk, or an exhausted hop budget.
/// - `ENOTDIR` when [`DestLeafKind::Directory`] was asked for and the leaf
///   is not one.
/// - `ENOENT` when the leaf does not exist beneath `root`.
/// - Any other I/O error from the underlying syscalls, forwarded verbatim.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/rsync.c:573-599` `set_file_attrs()`'s secure re-pin, and
///   the `xattr_refuse` fallback when the re-pin also fails
/// - `rsync-3.5.0/xattrs.c:386-390` `fsetxattr` vs `lsetxattr` on `fd >= 0`
pub fn pin_dest_leaf_confined(
    root: &Path,
    relative: &Path,
    kind: DestLeafKind,
) -> io::Result<File> {
    validate_relative(relative)?;
    imp::pin_dest_leaf_confined(root, relative, kind)
}

/// Reads the symlink target of `root`/`relative` with the parent components
/// resolved confined beneath `root`.
///
/// This is the file-list counterpart to [`open_source_confined`]: the sender
/// records a symlink's target in the flist, and a path-based `readlink` re-walks
/// every parent component at call time, so a parent raced into a symlink
/// pointing out of the module redirects the read and the *outside* link's target
/// is what goes on the wire. Resolving the parent once through the confined walk
/// and reading the leaf with `readlinkat` against the held fd closes that window:
/// the fd cannot be redirected after the fact.
///
/// The leaf is never followed - a `readlink` reads the link itself - so unlike
/// the source open there is no leaf policy to choose.
///
/// # Errors
///
/// - `EINVAL` when `relative` is absolute, contains a `..` component, or names
///   something that is not a symlink (the `readlink` contract).
/// - `EISDIR` when `relative` has no leaf: it is empty, all slashes, or its
///   final component is `.` or `..`.
/// - `ELOOP` when the parent walk refuses a symlink - an absolute or escaping
///   target, or an exhausted hop budget.
/// - `ENOENT` when the leaf does not exist beneath `root`.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/flist.c:247-255` `scan_readlink()` - reads the leaf with
///   `do_readlink_atfd(scan_dirfd, ...)` against the already-open scan dir.
/// - `rsync-3.5.0/flist.c:2028-2059` `secure_opendir()` - the daemon arm that
///   obtains that dirfd through `secure_relative_open_at()`, which is what makes
///   the subsequent `readlinkat` race-free.
/// - `rsync-3.5.0/util1.c:1216` `change_dir()` - the same confinement applied to
///   the per-argument directory descent, so a named `dir/link` operand is
///   covered even when no directory scan is running.
pub fn read_link_confined(root: &Path, relative: &Path) -> io::Result<std::path::PathBuf> {
    validate_relative(relative)?;
    imp::read_link_confined(root, relative)
}

/// Rejects an absolute `relative` or any `..` component up front with
/// `EINVAL`, matching upstream `secure_relative_open`'s portable front-door
/// check (`path_has_dotdot_component`). `.` and normal components are
/// allowed; the kernel / walk adjudicates the rest.
pub(crate) fn validate_relative(relative: &Path) -> io::Result<()> {
    for comp in relative.components() {
        match comp {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::from_raw_os_error(EINVAL));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
const EINVAL: i32 = libc::EINVAL;

// On non-Unix the module resolves through the `Unsupported` error path
// before ever validating, but the constant is still referenced.
#[cfg(not(unix))]
const EINVAL: i32 = 22;

#[cfg(unix)]
mod imp {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    use crate::dir_sandbox::at_syscalls::readlinkat;
    use crate::dir_sandbox::{ConfinePolicy, DirSandbox, openat};

    /// Ceiling on the symlink chain [`LeafPolicy::FollowConfined`] will
    /// resolve before giving up with `ELOOP`.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/sender.c:258` `int hops = 32;`
    const COPYLINKS_MAXHOPS: u32 = 32;

    pub(super) fn open_source_confined(
        root: &Path,
        relative: &Path,
        leaf: LeafPolicy,
        noatime: bool,
    ) -> io::Result<File> {
        // Classify the final component ONCE, before the platform split, and
        // hand both arms the slash-trimmed path. Doing it per-arm let them
        // disagree: `openat2` opens a directory named by a bare `.` and
        // reports `ENOENT` for an empty path, where upstream's walk reports
        // `EISDIR` for both. The classification is a property of the input,
        // not of the kernel.
        let (trimmed, _, _) = split_leaf(relative)?;

        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // Open the module root as the anchoring dirfd. The root is an
            // absolute, operator-trusted path, so a plain open (following any
            // symlinks in the root itself) matches upstream's
            // `openat(AT_FDCWD, basedir, O_RDONLY | O_DIRECTORY)`.
            use std::os::fd::AsFd;
            use std::os::unix::fs::OpenOptionsExt;
            let root_dir = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(root)?;
            if let Some(file) = linux::openat2_confined(root_dir.as_fd(), trimmed, leaf, noatime)? {
                return Ok(file);
            }
        }

        match leaf {
            LeafPolicy::Nofollow => walk_then_open_leaf(root, trimmed, noatime),
            LeafPolicy::FollowConfined => walk_following_leaf(root, trimmed, noatime),
        }
    }

    pub(super) fn pin_dest_leaf_confined(
        root: &Path,
        relative: &Path,
        kind: DestLeafKind,
    ) -> io::Result<File> {
        // Deliberately the shared walk on every platform, with no `openat2`
        // fast path: `openat2_confined` is shaped around the source open's
        // read-a-regular-file semantics, and a second entry point into it
        // would be a second place for the two arms to disagree. The walk is
        // the same resolver either way - only the syscall count differs, and
        // this is a once-per-file metadata pin, not the data path.
        let (_, dir, leaf) = split_leaf(relative)?;
        let parent = resolve_parent(root, dir)?;
        // upstream: rsync.c:596 - O_RDONLY|O_NOFOLLOW|O_NONBLOCK|O_NOCTTY|
        // O_CLOEXEC, plus O_DIRECTORY for a directory leaf. O_NONBLOCK is
        // load-bearing: without it, pinning a FIFO blocks until a writer
        // appears. O_NOCTTY keeps a raced device leaf from becoming this
        // process's controlling terminal.
        let mut flags =
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;
        if kind == DestLeafKind::Directory {
            flags |= libc::O_DIRECTORY;
        }
        openat(parent.root_dirfd(), leaf, flags, 0)
    }

    pub(super) fn read_link_confined(
        root: &Path,
        relative: &Path,
    ) -> io::Result<std::path::PathBuf> {
        let (_, dir, leaf) = split_leaf(relative)?;
        let parent = resolve_parent(root, dir)?;
        readlinkat(parent.root_dirfd(), leaf)
    }

    /// Resolve `relative`'s parent through the shared confined resolver, then
    /// open the leaf `O_NOFOLLOW` against it.
    ///
    /// The resolver follows in-tree directory symlinks and refuses absolute
    /// targets, `..` above the anchor, and anything the oracle excludes -
    /// exactly `ds_descend()`. `O_NOFOLLOW` then governs the leaf alone.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/sender.c:209-247` `sender_open_confined()`
    /// - `rsync-3.5.0/syscall.c:3037-3057` `secure_walk_at()`'s file-leaf arm
    fn walk_then_open_leaf(root: &Path, relative: &Path, noatime: bool) -> io::Result<File> {
        let (_, dir, leaf) = split_leaf(relative)?;
        let parent = resolve_parent(root, dir)?;
        open_leaf(parent.root_dirfd(), leaf, noatime)
    }

    /// Resolve a symlinked leaf deliberately, still confined beneath `root`.
    ///
    /// Reads the link, refuses an absolute target (it names a path the module
    /// does not contain, even when it happens to resolve back inside), and
    /// re-resolves a relative target through the walk - which pops `..` on its
    /// held-fd stack and refuses a pop above the anchor. The final open is
    /// still `O_NOFOLLOW`, so a flip raced in at the resolved leaf is refused.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/sender.c:250-330` `sender_open_copylinks_confined()`
    fn walk_following_leaf(root: &Path, relative: &Path, noatime: bool) -> io::Result<File> {
        let mut cur = relative.to_path_buf();
        for _ in 0..COPYLINKS_MAXHOPS {
            let (_, dir, leaf) = split_leaf(&cur)?;
            let parent = resolve_parent(root, dir)?;
            let target = match readlinkat(parent.root_dirfd(), leaf) {
                Ok(target) => target,
                // EINVAL means "not a symlink", so this is the resolved
                // target file. upstream: sender.c:308-317.
                Err(error) if error.raw_os_error() == Some(libc::EINVAL) => {
                    return open_leaf(parent.root_dirfd(), leaf, noatime);
                }
                Err(error) => return Err(error),
            };
            if target.as_os_str().is_empty() || target.is_absolute() {
                return Err(io::Error::from_raw_os_error(libc::ELOOP));
            }
            cur = dir.join(target);
        }
        Err(io::Error::from_raw_os_error(libc::ELOOP))
    }

    /// Walk `dir` beneath `root` through the shared per-component resolver.
    ///
    /// The anchor *is* the confinement root here, and the walk already refuses
    /// every pop above its anchor and every absolute symlink target, so the
    /// exclude oracle would be redundant. Upstream carries one because
    /// `secure_walk_at()` is shared with call sites anchored *inside* the
    /// module (`basedir == NULL`, i.e. the cwd); this call site is not one of
    /// them.
    fn resolve_parent(root: &Path, dir: &Path) -> io::Result<DirSandbox> {
        DirSandbox::open_dest_anchor_confined(root, dir, ConfinePolicy::operator_trusted())
    }

    /// Split `relative` into `(trimmed, parent, leaf)` on raw `/` bytes.
    ///
    /// Deliberately not `Path::components()`: that normalises, and the rule
    /// being mirrored is a byte split. Trailing slashes are trimmed first so
    /// the last-component test is exact - and `trimmed` is handed to the
    /// kernel arm too, which would otherwise see `sub/data/` and report
    /// `ENOTDIR` where upstream opens the file.
    ///
    /// # Errors
    ///
    /// `EISDIR` when there is no file leaf to open: an empty or all-slash
    /// path, or a final `.` / `..`. Those name a directory, and upstream
    /// reports `EISDIR` for a caller that did not ask for `O_DIRECTORY`.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/sender.c:212-226` `sender_open_confined()`'s `strrchr`
    /// - `rsync-3.5.0/syscall.c:3002-3006` `secure_walk_at()`'s slash trim
    /// - `rsync-3.5.0/syscall.c:3025-3033` a final `.` / `..` -> `EISDIR`
    /// - `rsync-3.5.0/syscall.c:3079-3086` no component at all -> `EISDIR`
    fn split_leaf(relative: &Path) -> io::Result<(&Path, &Path, &OsStr)> {
        let bytes = relative.as_os_str().as_bytes();
        let end = bytes.iter().rposition(|byte| *byte != b'/');
        let bytes = match end {
            Some(last) => &bytes[..=last],
            // Empty, or nothing but slashes: no leaf, so this names the anchor
            // directory itself.
            None => return Err(io::Error::from_raw_os_error(libc::EISDIR)),
        };
        let (dir, leaf) = match bytes.iter().rposition(|byte| *byte == b'/') {
            Some(slash) => (&bytes[..slash], &bytes[slash + 1..]),
            None => (&bytes[..0], bytes),
        };
        if leaf == b"." || leaf == b".." {
            // A movement, not a name to open.
            return Err(io::Error::from_raw_os_error(libc::EISDIR));
        }
        Ok((
            Path::new(OsStr::from_bytes(bytes)),
            Path::new(OsStr::from_bytes(dir)),
            OsStr::from_bytes(leaf),
        ))
    }

    /// Opens the leaf `O_RDONLY | O_NOFOLLOW`, honouring `--open-noatime`
    /// with the same graceful fallback the plain source open uses.
    fn open_leaf(
        parent: std::os::fd::BorrowedFd<'_>,
        leaf: &std::ffi::OsStr,
        noatime: bool,
    ) -> io::Result<File> {
        let base = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        if let Some(extra) = noatime_flag(noatime) {
            match openat(parent, leaf, base | extra, 0) {
                Ok(file) => return Ok(file),
                Err(error) if noatime_retryable(&error) => {}
                Err(error) => return Err(error),
            }
        }
        openat(parent, leaf, base, 0)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(super) mod linux {
        use super::{LeafPolicy, noatime_flag, noatime_retryable};
        use std::ffi::CString;
        use std::fs::File;
        use std::io;
        use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;

        use crate::linux_capabilities::openat2_supported;

        /// Attempts `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` for
        /// `relative` beneath `root_fd`.
        ///
        /// `O_NOFOLLOW` is added for [`LeafPolicy::Nofollow`]. The kernel
        /// applies it to the *final* component only, so intermediate in-tree
        /// symlinks are still followed - the same division upstream builds by
        /// hand out of `ds_descend()` plus an `O_NOFOLLOW` leaf open. Without
        /// it, `RESOLVE_BENEATH` resolves the leaf symlink too, which is what
        /// `LeafPolicy::FollowConfined` wants and refuses absolute targets on
        /// the kernel's behalf.
        ///
        /// Returns `Ok(Some(file))` on success, `Ok(None)` when the kernel
        /// lacks `openat2` (`ENOSYS`, cached by [`openat2_supported`]) so the
        /// caller falls back to the portable walk, and `Err(_)` for every
        /// other failure - including the deliberate confinement refusals
        /// (`EXDEV` for an escape, `ELOOP` for a magic link or a refused leaf
        /// symlink) the caller must surface.
        pub(super) fn openat2_confined(
            root_fd: BorrowedFd<'_>,
            relative: &Path,
            leaf: LeafPolicy,
            noatime: bool,
        ) -> io::Result<Option<File>> {
            if !openat2_supported() {
                return Ok(None);
            }

            let c_rel = CString::new(relative.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path contains interior null byte",
                )
            })?;

            let mut base = libc::O_RDONLY | libc::O_CLOEXEC;
            if leaf == LeafPolicy::Nofollow {
                base |= libc::O_NOFOLLOW;
            }
            if let Some(extra) = noatime_flag(noatime) {
                match raw_openat2(root_fd, &c_rel, base | extra) {
                    Ok(outcome) => return Ok(outcome),
                    Err(error) if noatime_retryable(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            raw_openat2(root_fd, &c_rel, base)
        }

        /// Issues the raw `openat2(2)` syscall with
        /// `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS`.
        fn raw_openat2(
            root_fd: BorrowedFd<'_>,
            c_rel: &CString,
            flags: i32,
        ) -> io::Result<Option<File>> {
            // SAFETY: this block performs three FFI touches, mirroring the
            // audited pattern in `dir_sandbox::openat2_beneath`:
            //
            // 1. `std::mem::zeroed::<open_how>()` - `libc::open_how` is
            //    `#[non_exhaustive]` and `repr(C)` with integer-only fields;
            //    an all-zero bit pattern is the documented "no constraint"
            //    default for every `openat2(2)` knob.
            // 2. `libc::syscall(SYS_openat2, root_fd, c_rel, &how, size)` -
            //    `root_fd.as_raw_fd()` is a live borrowed fd that outlives
            //    the call; `c_rel` is a valid NUL-terminated C string
            //    borrowed for the call; `how` is fully initialised and its
            //    address plus `size_of::<open_how>()` are handed to the
            //    kernel per the syscall ABI. The kernel retains none of the
            //    pointers past return.
            // 3. `File::from_raw_fd(raw)` - takes exclusive ownership of the
            //    fresh `O_CLOEXEC` fd; it is never duplicated, leaked, or
            //    aliased elsewhere.
            #[allow(unsafe_code)]
            let raw = unsafe {
                let mut how: libc::open_how = std::mem::zeroed();
                how.flags = flags as u64;
                how.mode = 0;
                how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS;

                libc::syscall(
                    libc::SYS_openat2,
                    root_fd.as_raw_fd(),
                    c_rel.as_ptr(),
                    &how as *const libc::open_how,
                    std::mem::size_of::<libc::open_how>(),
                )
            };

            if raw >= 0 {
                // SAFETY: `raw` is a non-negative fd just returned by
                // `openat2(2)` with `O_CLOEXEC`, owned exclusively here.
                #[allow(unsafe_code)]
                let file = unsafe { File::from_raw_fd(raw as libc::c_int) };
                return Ok(Some(file));
            }

            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOSYS) {
                return Ok(None);
            }
            Err(err)
        }
    }

    /// Returns `O_NOATIME` when `noatime` is requested on Linux/Android,
    /// `None` otherwise (the flag is undefined on other targets).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn noatime_flag(noatime: bool) -> Option<i32> {
        noatime.then_some(libc::O_NOATIME)
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn noatime_flag(_noatime: bool) -> Option<i32> {
        None
    }

    /// Whether an `O_NOATIME` open failure should be retried without the
    /// flag. Mirrors `open_source::try_open_noatime`.
    fn noatime_retryable(error: &io::Error) -> bool {
        matches!(
            error.raw_os_error(),
            Some(libc::EPERM | libc::EACCES | libc::EINVAL | libc::ENOTSUP | libc::EROFS)
        )
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    pub(super) fn open_source_confined(
        _root: &Path,
        _relative: &Path,
        _leaf: LeafPolicy,
        _noatime: bool,
    ) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "confined source open is Unix-only (the daemon sender is Unix-only)",
        ))
    }

    pub(super) fn pin_dest_leaf_confined(
        _root: &Path,
        _relative: &Path,
        _kind: DestLeafKind,
    ) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "confined destination leaf pin is Unix-only (it exists for fd-based xattr/ACL writes)",
        ))
    }

    pub(super) fn read_link_confined(
        _root: &Path,
        _relative: &Path,
    ) -> io::Result<std::path::PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "confined readlink is Unix-only (the daemon sender is Unix-only)",
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    fn read_to_string(mut file: File) -> String {
        let mut buf = String::new();
        file.read_to_string(&mut buf).expect("read");
        buf
    }

    #[test]
    fn opens_regular_file_beneath_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("sub")).expect("mkdir sub");
        std::fs::write(root.join("sub/data"), b"payload").expect("write");

        let file = open_source_confined(&root, Path::new("sub/data"), LeafPolicy::Nofollow, false)
            .expect("open");
        assert_eq!(read_to_string(file), "payload");
    }

    /// An in-tree symlinked *directory* component is followed on every
    /// platform.
    ///
    /// WHY: upstream's `ds_descend()` follows a relative in-tree symlink
    /// (`rsync-3.5.0/syscall.c:2937-2961`) - the behaviour restored for issue
    /// #715, so a module whose layout uses a symlinked subdirectory keeps
    /// serving. Before this walk was routed through the shared resolver the
    /// portable arm opened every component `O_NOFOLLOW` and refused, which
    /// made an ordinary module unservable on macOS/BSD while Linux served it.
    ///
    /// The previous version of this test accepted refusal *or* success, so it
    /// could not fail on either arm. It now pins one answer for both.
    #[test]
    fn an_in_tree_symlinked_directory_component_is_followed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("real")).expect("mkdir real");
        std::fs::write(root.join("real/data"), b"in-tree").expect("write");
        symlink("real", root.join("link")).expect("symlink");

        let file = open_source_confined(&root, Path::new("link/data"), LeafPolicy::Nofollow, false)
            .expect("an in-tree symlinked directory component must be followed");
        assert_eq!(read_to_string(file), "in-tree");
    }

    /// A symlinked *leaf* is refused under the default policy even when its
    /// target is in-tree.
    ///
    /// WHY: upstream opens the file leaf `O_NOFOLLOW`
    /// (`rsync-3.5.0/syscall.c:3050`, `sender.c:245`) so an attacker who wins
    /// the race between the file-list scan and the content open cannot swap
    /// the leaf for a symlink and redirect the read. Confinement alone does
    /// not close that: the swapped target may be a *different in-module file*
    /// the peer was never authorised to see.
    ///
    /// This is the arm that regressed silently: `openat2(RESOLVE_BENEATH)`
    /// without `O_NOFOLLOW` follows the leaf, so on Linux the defence was
    /// absent while the portable walk refused - and no test looked at the
    /// distinction.
    #[test]
    fn a_symlinked_leaf_is_refused_under_nofollow() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::write(root.join("real"), b"in-tree").expect("write");
        symlink("real", root.join("link")).expect("symlink");

        let err = open_source_confined(&root, Path::new("link"), LeafPolicy::Nofollow, false)
            .expect_err("O_NOFOLLOW must refuse the leaf even for an in-tree target");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::ELOOP),
            "expected ELOOP for a refused leaf symlink, got: {err}"
        );
    }

    /// `--copy-links` resolves that same leaf, still beneath the root.
    ///
    /// WHY: upstream keeps a second entry point rather than dropping the
    /// `O_NOFOLLOW` (`rsync-3.5.0/sender.c:250-330`
    /// `sender_open_copylinks_confined`), because a symlink-following mode is
    /// an operator instruction that must not be silently downgraded. Collapse
    /// the two and either `-L` stops working or the raced-leaf defence above
    /// disappears.
    #[test]
    fn a_symlinked_leaf_is_resolved_under_follow_confined() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("sub")).expect("mkdir sub");
        std::fs::write(root.join("sub/real"), b"followed").expect("write");
        symlink("real", root.join("sub/link")).expect("symlink");

        let file = open_source_confined(
            &root,
            Path::new("sub/link"),
            LeafPolicy::FollowConfined,
            false,
        )
        .expect("copy-links must resolve an in-tree leaf symlink");
        assert_eq!(read_to_string(file), "followed");
    }

    /// A symlink chain is resolved to its end, then opened.
    ///
    /// WHY: upstream loops rather than resolving one hop
    /// (`rsync-3.5.0/sender.c:265-330`), and each hop is re-anchored, so a
    /// chain cannot walk out of the module one link at a time.
    #[test]
    fn follow_confined_resolves_a_symlink_chain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("a")).expect("mkdir a");
        std::fs::write(root.join("a/target"), b"end-of-chain").expect("write");
        symlink("target", root.join("a/mid")).expect("symlink mid");
        symlink("a/mid", root.join("head")).expect("symlink head");

        let file =
            open_source_confined(&root, Path::new("head"), LeafPolicy::FollowConfined, false)
                .expect("a chain of in-tree links must resolve");
        assert_eq!(read_to_string(file), "end-of-chain");
    }

    /// An absolute leaf target is refused under `--copy-links`.
    ///
    /// WHY: upstream refuses an absolute target outright
    /// (`rsync-3.5.0/sender.c:320-323`) rather than testing where it lands -
    /// it names a path the module does not contain, even when it happens to
    /// resolve back inside.
    #[test]
    fn follow_confined_refuses_an_absolute_leaf_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let root = base.join("module");
        std::fs::create_dir(&root).expect("mkdir module");
        std::fs::write(base.join("secret"), b"do-not-leak").expect("write secret");
        symlink(base.join("secret"), root.join("leak")).expect("symlink leak");

        let err = open_source_confined(&root, Path::new("leak"), LeafPolicy::FollowConfined, false)
            .expect_err("an absolute leaf target must be refused");
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::EXDEV),
            "expected ELOOP or EXDEV for an absolute leaf target, got: {err}"
        );
    }

    /// A relative leaf target that climbs out of the module is refused.
    ///
    /// WHY: this is the case an absolute-target check alone would miss, and
    /// the one a naive "join and open" would let through. The walk resolves
    /// `..` by popping its held-fd stack and refuses a pop above the anchor
    /// (`rsync-3.5.0/syscall.c:2896-2901`), so the climb cannot be laundered
    /// through a symlink.
    #[test]
    fn follow_confined_refuses_a_relative_leaf_target_that_climbs_out() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let root = base.join("module");
        std::fs::create_dir(&root).expect("mkdir module");
        std::fs::write(base.join("secret"), b"do-not-leak").expect("write secret");
        symlink("../secret", root.join("leak")).expect("symlink leak");

        let err = open_source_confined(&root, Path::new("leak"), LeafPolicy::FollowConfined, false)
            .expect_err("a climbing relative leaf target must be refused");
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::EXDEV),
            "expected ELOOP or EXDEV for a climbing leaf target, got: {err}"
        );
    }

    /// A self-referential symlink terminates instead of spinning.
    ///
    /// WHY: upstream caps the chain at 32 hops (`rsync-3.5.0/sender.c:258`).
    /// Without the cap a module operator - or an attacker with write access
    /// to the tree - hangs the daemon's sender with two symlinks.
    #[test]
    fn follow_confined_bounds_the_symlink_chain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        symlink("pong", root.join("ping")).expect("symlink ping");
        symlink("ping", root.join("pong")).expect("symlink pong");

        let err = open_source_confined(&root, Path::new("ping"), LeafPolicy::FollowConfined, false)
            .expect_err("a symlink cycle must terminate with an error");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::ELOOP),
            "expected ELOOP for an exhausted hop budget, got: {err}"
        );
    }

    #[test]
    fn refuses_escape_via_directory_symlink() {
        // A directory component symlinked to a sibling OUTSIDE the root must
        // not let the open escape. This is the core TOCTOU defence.
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let root = base.join("module");
        std::fs::create_dir(&root).expect("mkdir module");
        let outside = base.join("outside");
        std::fs::create_dir(&outside).expect("mkdir outside");
        std::fs::write(outside.join("secret"), b"do-not-leak").expect("write secret");

        // `<root>/escape` -> the absolute `outside` dir (outside the module).
        symlink(&outside, root.join("escape")).expect("symlink escape");

        let err = open_source_confined(
            &root,
            Path::new("escape/secret"),
            LeafPolicy::Nofollow,
            false,
        )
        .expect_err("escape must be refused");
        // openat2 rejects the escape with EXDEV; the resolver refuses the
        // absolute symlink target with ELOOP. ENOTDIR is deliberately no
        // longer accepted - that was the old `O_NOFOLLOW`-every-component
        // walk, which refused the component for the wrong reason.
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::EXDEV) || code == Some(libc::ELOOP),
            "expected EXDEV or ELOOP for an escape, got: {err}"
        );
    }

    #[test]
    fn refuses_symlinked_leaf_pointing_outside() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let root = base.join("module");
        std::fs::create_dir(&root).expect("mkdir module");
        std::fs::write(base.join("secret"), b"do-not-leak").expect("write secret");

        // Absolute symlink leaf targeting a file outside the module.
        symlink(base.join("secret"), root.join("leak")).expect("symlink leak");

        let err = open_source_confined(&root, Path::new("leak"), LeafPolicy::Nofollow, false)
            .expect_err("symlinked leaf pointing outside must be refused");
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::EXDEV) || code == Some(libc::ELOOP),
            "expected EXDEV or ELOOP for an outside leaf, got: {err}"
        );
    }

    #[test]
    fn rejects_absolute_relative_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let err =
            open_source_confined(&root, Path::new("/etc/passwd"), LeafPolicy::Nofollow, false)
                .expect_err("absolute relative path must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn rejects_dotdot_component() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let err = open_source_confined(&root, Path::new("../secret"), LeafPolicy::Nofollow, false)
            .expect_err("dotdot component must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
    }

    /// The front-door `..` rejection applies to the *caller's* input only.
    ///
    /// WHY: upstream skips it for a path it re-anchored itself
    /// (`rsync-3.5.0/syscall.c:3153-3167`, the `reanchored` guard) because a
    /// followed symlink's target legitimately contains parent-relative
    /// components - `secure_relative_open_at_beneath()` exists for exactly
    /// that. Applying the front-door check to a derived path would make
    /// ordinary in-module links unfollowable.
    #[test]
    fn a_followed_target_may_contain_dotdot_while_caller_input_may_not() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("here")).expect("mkdir here");
        std::fs::create_dir(root.join("there")).expect("mkdir there");
        std::fs::write(root.join("there/data"), b"sibling").expect("write");
        // Climbs to the module root and back down - inside the whole way.
        symlink("../there/data", root.join("here/link")).expect("symlink");

        let file = open_source_confined(
            &root,
            Path::new("here/link"),
            LeafPolicy::FollowConfined,
            false,
        )
        .expect("an in-module `..` target must resolve");
        assert_eq!(read_to_string(file), "sibling");

        // The same `..` written by the caller is still rejected up front.
        let err = open_source_confined(
            &root,
            Path::new("here/../there/data"),
            LeafPolicy::FollowConfined,
            false,
        )
        .expect_err("caller-supplied dotdot must still be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn missing_file_reports_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let err = open_source_confined(&root, Path::new("nope"), LeafPolicy::Nofollow, false)
            .expect_err("missing file must fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    /// A path naming a directory is refused with `EISDIR`, and a trailing
    /// slash is trimmed rather than being fatal - identically on both
    /// platform arms.
    ///
    /// WHY: the final-component rule is a property of the input, not of the
    /// kernel, but it was first written inside the portable arm only. The two
    /// arms then disagreed on every input here: `openat2` reports `ENOENT`
    /// for an empty path, opens the directory named by a bare `.`, and
    /// reports `ENOTDIR` for `sub/data/` - where upstream's
    /// `secure_walk_at()` reports `EISDIR`, `EISDIR`, and the file. Caught
    /// only by running the suite on Linux; all four pass on macOS either way,
    /// which is exactly why the classification now happens once, above the
    /// platform split.
    #[test]
    fn the_leaf_classification_is_identical_on_both_platform_arms() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("sub")).expect("mkdir sub");
        std::fs::write(root.join("sub/data"), b"payload").expect("write");

        for directory in ["", ".", "sub/."] {
            let err =
                open_source_confined(&root, Path::new(directory), LeafPolicy::Nofollow, false)
                    .expect_err("a path naming a directory must be refused");
            assert_eq!(
                err.raw_os_error(),
                Some(libc::EISDIR),
                "expected EISDIR for {directory:?}, got: {err}"
            );
        }

        // A leading slash is still the front door's EINVAL, not EISDIR - it is
        // tested before the leaf is classified, exactly as upstream tests
        // `relpath[0] == '/'` before entering the walk (syscall.c:3105-3109).
        for absolute in ["/", "///"] {
            let err = open_source_confined(&root, Path::new(absolute), LeafPolicy::Nofollow, false)
                .expect_err("an absolute path must be refused");
            assert_eq!(
                err.raw_os_error(),
                Some(libc::EINVAL),
                "expected EINVAL for {absolute:?}, got: {err}"
            );
        }

        // Trailing slashes are trimmed, so the file still opens.
        let file = open_source_confined(&root, Path::new("sub/data/"), LeafPolicy::Nofollow, false)
            .expect("a trailing slash must be trimmed, not fatal");
        assert_eq!(read_to_string(file), "payload");
    }

    /// Builds `root/{sub -> outside, real}` plus an outside sentinel, and
    /// returns `(root, outside)`. `sub` is the parent component an attacker
    /// flips; `real` is the legitimate in-tree directory symlink target that
    /// the follow-the-parent rule must keep working.
    fn dest_tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir(&root).expect("mkdir root");
        std::fs::create_dir(&outside).expect("mkdir outside");
        std::fs::write(outside.join("victim"), b"outside").expect("write victim");
        (tmp, root, outside)
    }

    /// The defect this exists for: a parent component flipped to a symlink
    /// pointing OUT of the tree must not yield a pin on the outside file.
    /// A path-based `lsetxattr` would follow it, because `lsetxattr` only
    /// declines to follow the *leaf*.
    #[test]
    fn a_parent_flipped_outside_the_root_is_refused() {
        let (_tmp, root, outside) = dest_tree();
        symlink(&outside, root.join("sub")).expect("plant escaping parent");

        let error =
            pin_dest_leaf_confined(&root, Path::new("sub/victim"), DestLeafKind::NonDirectory)
                .expect_err("an escaping parent must refuse the pin");
        assert!(
            !outside.join("victim").is_symlink(),
            "the sentinel must still be the real file the refusal protected"
        );
        assert_ne!(
            error.kind(),
            io::ErrorKind::NotFound,
            "a refusal, not a missing file: got {error:?}"
        );
    }

    /// Non-vacuity companion, and the regression this pin's first attempt
    /// caused: a RELATIVE in-tree directory symlink is legitimate (it is
    /// what `--keep-dirlinks` transfers into) and upstream's `ds_descend()`
    /// follows it. A refuse-all walk would break ordinary local copies, so
    /// the escape test above must not be passing for that reason.
    #[test]
    fn a_relative_in_tree_parent_symlink_is_followed() {
        let (_tmp, root, _outside) = dest_tree();
        std::fs::create_dir(root.join("real")).expect("mkdir real");
        std::fs::write(root.join("real/data"), b"payload").expect("write");
        symlink("real", root.join("sub")).expect("plant in-tree parent");

        let file = pin_dest_leaf_confined(&root, Path::new("sub/data"), DestLeafKind::NonDirectory)
            .expect("an in-tree relative parent symlink must still resolve");
        assert_eq!(read_to_string(file), "payload");
    }

    /// The leaf rule is `O_NOFOLLOW`: a leaf raced into a symlink is refused
    /// even when it points back inside the tree, because the pin exists to
    /// hold the inode the receiver just wrote, not whatever the name means
    /// now.
    #[test]
    fn a_symlinked_leaf_is_refused() {
        let (_tmp, root, _outside) = dest_tree();
        std::fs::write(root.join("target"), b"payload").expect("write");
        symlink("target", root.join("leaf")).expect("plant leaf symlink");

        let error = pin_dest_leaf_confined(&root, Path::new("leaf"), DestLeafKind::NonDirectory)
            .expect_err("a symlinked leaf must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "got {error:?}");
    }

    /// `DestLeafKind::Directory` adds `O_DIRECTORY`, so a leaf that is not a
    /// directory is refused rather than pinned - upstream derives the flag
    /// from the INTENDED mode precisely so a raced non-directory cannot slip
    /// through.
    #[test]
    fn a_directory_kind_refuses_a_non_directory_leaf() {
        let (_tmp, root, _outside) = dest_tree();
        std::fs::create_dir(root.join("adir")).expect("mkdir");
        std::fs::write(root.join("afile"), b"payload").expect("write");

        pin_dest_leaf_confined(&root, Path::new("adir"), DestLeafKind::Directory)
            .expect("a real directory must pin");
        let error = pin_dest_leaf_confined(&root, Path::new("afile"), DestLeafKind::Directory)
            .expect_err("a non-directory must be refused under Directory");
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR), "got {error:?}");
    }

    /// A FIFO must pin without blocking. `O_NONBLOCK` is the reason, and
    /// dropping it from the flag set would hang this test rather than fail
    /// it - which is why the assertion is that the call returns at all.
    #[test]
    fn a_fifo_leaf_pins_without_a_writer() {
        let (_tmp, root, _outside) = dest_tree();
        let fifo = root.join("pipe");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("cstring");
        // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
        // call, and `mkfifo` only reads it.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo: {}", io::Error::last_os_error());

        pin_dest_leaf_confined(&root, Path::new("pipe"), DestLeafKind::NonDirectory)
            .expect("a FIFO leaf must pin without waiting for a writer");
    }

    /// Front-door validation is shared with the source open, so a `..` never
    /// reaches the walk.
    #[test]
    fn a_dotdot_component_is_rejected_before_the_walk() {
        let (_tmp, root, _outside) = dest_tree();
        let error = pin_dest_leaf_confined(
            &root,
            Path::new("../outside/victim"),
            DestLeafKind::NonDirectory,
        )
        .expect_err("`..` must be rejected");
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL), "got {error:?}");
    }

    /// A symlink read through the confined helper must return the in-module
    /// target, so the escape tests below discriminate rather than pass because
    /// the helper refuses everything.
    #[test]
    fn read_link_confined_reads_an_in_module_link() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("real")).expect("mkdir real");
        symlink("inside-target", root.join("real/link")).expect("symlink");

        let target = read_link_confined(&root, Path::new("real/link")).expect("read the link");
        assert_eq!(target, PathBuf::from("inside-target"));
    }

    /// THE DEFECT: a parent component raced into a symlink pointing out of the
    /// module must not redirect the read.
    ///
    /// WHY: the sender records this target in the file list, so following the
    /// raced parent puts the *outside* link's target on the wire - an
    /// information leak from a tree the module does not contain. Upstream
    /// resolves the parent once through `secure_relative_open()` and reads the
    /// leaf against the held fd (`flist.c:247-255`, `util1.c:1216`); a
    /// path-based `readlink` re-walks the parent on every call and has no such
    /// window closed.
    #[test]
    fn read_link_confined_refuses_a_parent_symlink_escaping_the_root() {
        let base = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(base.path()).expect("canonicalize");
        let root = base.join("module");
        std::fs::create_dir(&root).expect("mkdir module");
        let outside = base.join("outside");
        std::fs::create_dir(&outside).expect("mkdir outside");
        symlink("OUTSIDE-LINK-TARGET", outside.join("link")).expect("outside link");
        // The raced flip: the module's `real` directory becomes a symlink out.
        symlink(&outside, root.join("real")).expect("plant the escape");

        let error = read_link_confined(&root, Path::new("real/link"))
            .expect_err("the confined read must refuse the escaping parent");
        let code = error.raw_os_error();
        assert!(
            code == Some(libc::EXDEV) || code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "expected a confinement refusal, got: {error:?}"
        );
    }

    /// An in-tree directory symlink is still followed, so a legitimate module
    /// layout keeps working.
    ///
    /// WHY: upstream's walk refuses escapes, not symlinks (`ds_descend`,
    /// `syscall.c:2961`). A helper that refused every symlinked parent would
    /// break ordinary modules while passing the escape test above - which is
    /// exactly the over-refusal this pins against.
    #[test]
    fn read_link_confined_follows_an_in_tree_directory_symlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("real")).expect("mkdir real");
        symlink("in-module-target", root.join("real/link")).expect("symlink");
        symlink("real", root.join("alias")).expect("in-tree dir symlink");

        let target = read_link_confined(&root, Path::new("alias/link"))
            .expect("an in-tree directory symlink must still be followed");
        assert_eq!(target, PathBuf::from("in-module-target"));
    }

    /// A non-symlink leaf reports `EINVAL`, the `readlink(2)` contract the
    /// callers' error handling is written against.
    #[test]
    fn read_link_confined_reports_einval_for_a_non_symlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::write(root.join("plain"), b"not a link").expect("write");

        let error = read_link_confined(&root, Path::new("plain"))
            .expect_err("a regular file is not a symlink");
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL), "got {error:?}");
    }
}
