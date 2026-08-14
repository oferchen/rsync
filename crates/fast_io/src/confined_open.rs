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
/// - `EINVAL` when `relative` is absolute, empty, or contains a `..`
///   component (front-door validation, mirroring upstream
///   `secure_relative_open`).
/// - `EISDIR` when the final component names a directory rather than a file.
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

/// Rejects an absolute `relative` or any `..` component up front with
/// `EINVAL`, matching upstream `secure_relative_open`'s portable front-door
/// check (`path_has_dotdot_component`). `.` and normal components are
/// allowed; the kernel / walk adjudicates the rest.
fn validate_relative(relative: &Path) -> io::Result<()> {
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
            if let Some(file) = linux::openat2_confined(root_dir.as_fd(), relative, leaf, noatime)?
            {
                return Ok(file);
            }
        }

        match leaf {
            LeafPolicy::Nofollow => walk_then_open_leaf(root, relative, noatime),
            LeafPolicy::FollowConfined => walk_following_leaf(root, relative, noatime),
        }
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
        let (dir, leaf) = split_leaf(relative)?;
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
            let (dir, leaf) = split_leaf(&cur)?;
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

    /// Split `relative` into its parent path and final component on raw `/`
    /// bytes.
    ///
    /// Deliberately not `Path::components()`: that normalises, and the rule
    /// being mirrored is a byte split. Trailing slashes are trimmed first so
    /// the last-component test is exact.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/sender.c:212-226` `sender_open_confined()`'s `strrchr`
    /// - `rsync-3.5.0/syscall.c:3002-3006` `secure_walk_at()`'s slash trim
    fn split_leaf(relative: &Path) -> io::Result<(&Path, &OsStr)> {
        let bytes = relative.as_os_str().as_bytes();
        let end = bytes.iter().rposition(|byte| *byte != b'/');
        let bytes = match end {
            Some(last) => &bytes[..=last],
            // Empty, or nothing but slashes: no leaf to open beneath the root.
            None => return Err(io::Error::from_raw_os_error(libc::EINVAL)),
        };
        let (dir, leaf) = match bytes.iter().rposition(|byte| *byte == b'/') {
            Some(slash) => (&bytes[..slash], &bytes[slash + 1..]),
            None => (&bytes[..0], bytes),
        };
        if leaf == b"." || leaf == b".." {
            // A movement, not a name to open: the caller asked for a
            // directory. upstream: syscall.c:3025-3033 reports EISDIR when
            // O_DIRECTORY was not requested.
            return Err(io::Error::from_raw_os_error(libc::EISDIR));
        }
        Ok((Path::new(OsStr::from_bytes(dir)), OsStr::from_bytes(leaf)))
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
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::symlink;

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

    /// An empty or all-slash relative path has no leaf to open.
    #[test]
    fn rejects_a_relative_path_with_no_leaf() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        for empty in ["", "/"] {
            let err = open_source_confined(&root, Path::new(empty), LeafPolicy::Nofollow, false)
                .expect_err("a path with no leaf must be rejected");
            assert_eq!(
                err.raw_os_error(),
                Some(libc::EINVAL),
                "expected EINVAL for {empty:?}, got: {err}"
            );
        }
    }
}
