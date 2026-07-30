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
//! # Platform behaviour
//!
//! upstream: `syscall.c:secure_relative_open` (called from
//! `sender.c:secure_relative_open` when `use_secure_symlinks`, i.e. a
//! non-chroot daemon).
//!
//! - Linux 5.6+: `openat2(2)` with
//!   `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS` anchored at the module-root
//!   dirfd. In-tree symlinks are followed; escapes fail with `EXDEV`.
//! - Older Linux and every other Unix target: a per-component
//!   `O_NOFOLLOW` walk from the module-root dirfd, mirroring upstream's
//!   portable fallback. This is stricter than the kernel path - it refuses
//!   *every* symlink component, in-tree or not - trading function for
//!   safety exactly as upstream documents.
//! - Non-Unix: unsupported. The oc daemon (the only caller) is Unix-only,
//!   so this path is never reached; it returns an `Unsupported` error.

use std::fs::File;
use std::io;
use std::path::{Component, Path};

/// Opens `root`/`relative` for reading, confined beneath `root`.
///
/// `root` is the operator-trusted module-root directory (an absolute path
/// from the daemon config); it is opened with normal symlink resolution.
/// `relative` is the module-relative source path; its resolution is
/// confined so it cannot escape `root`.
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
/// - `EXDEV` (Linux `openat2` path) when resolution would escape `root`.
/// - `ELOOP` (portable fallback) when any component of `relative` is a
///   symlink.
/// - `ENOENT` when the file does not exist beneath `root`.
/// - Any other I/O error from the underlying syscalls, forwarded verbatim.
///
/// upstream: sender.c:secure_relative_open
pub fn open_source_confined(root: &Path, relative: &Path, noatime: bool) -> io::Result<File> {
    validate_relative(relative)?;
    imp::open_source_confined(root, relative, noatime)
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
    use std::os::fd::AsFd;
    use std::os::unix::fs::OpenOptionsExt;

    use crate::dir_sandbox::openat;

    pub(super) fn open_source_confined(
        root: &Path,
        relative: &Path,
        noatime: bool,
    ) -> io::Result<File> {
        // Open the module root as the anchoring dirfd. The root is an
        // absolute, operator-trusted path, so a plain open (following any
        // symlinks in the root itself) matches upstream's
        // `openat(AT_FDCWD, basedir, O_RDONLY | O_DIRECTORY)`.
        let root_dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(root)?;

        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            if let Some(file) = linux::openat2_confined(root_dir.as_fd(), relative, noatime)? {
                return Ok(file);
            }
        }

        walk_nofollow(root_dir.as_fd(), relative, noatime)
    }

    /// Portable per-component `O_NOFOLLOW` walk from `root_fd`, mirroring
    /// the fallback in upstream `secure_relative_open` for platforms
    /// without kernel `RESOLVE_BENEATH`. Every component - directory and
    /// leaf - is opened `O_NOFOLLOW`, so any symlink along the path is
    /// refused with `ELOOP`.
    fn walk_nofollow(
        root_fd: std::os::fd::BorrowedFd<'_>,
        relative: &Path,
        noatime: bool,
    ) -> io::Result<File> {
        let mut components: Vec<&std::ffi::OsStr> = Vec::new();
        for comp in relative.components() {
            if let Component::Normal(part) = comp {
                components.push(part);
            }
            // `.` is skipped; `..`, root, and prefix were rejected by
            // `validate_relative`.
        }

        let Some((leaf, dirs)) = components.split_last() else {
            // Empty relative path: nothing to open beneath the root.
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        };

        let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut current: Option<File> = None;
        for dir in dirs {
            let parent = current.as_ref().map_or(root_fd, AsFd::as_fd);
            let next = openat(parent, dir, dir_flags, 0)?;
            current = Some(next);
        }

        let parent = current.as_ref().map_or(root_fd, AsFd::as_fd);
        open_leaf(parent, leaf, noatime)
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
        use super::{noatime_flag, noatime_retryable};
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
        /// Returns `Ok(Some(file))` on success, `Ok(None)` when the kernel
        /// lacks `openat2` (`ENOSYS`, cached by [`openat2_supported`]) so the
        /// caller falls back to the portable walk, and `Err(_)` for every
        /// other failure - including the deliberate confinement refusals
        /// (`EXDEV` for an escape, `ELOOP` for a magic link) the caller must
        /// surface.
        pub(super) fn openat2_confined(
            root_fd: BorrowedFd<'_>,
            relative: &Path,
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

            let base = libc::O_RDONLY | libc::O_CLOEXEC;
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

        let file = open_source_confined(&root, Path::new("sub/data"), false).expect("open");
        assert_eq!(read_to_string(file), "payload");
    }

    #[test]
    fn follows_in_tree_directory_symlink_on_kernel_path() {
        // In-tree symlinks are allowed by the openat2 RESOLVE_BENEATH path
        // (upstream #715 behaviour). On the portable fallback walk this is
        // refused with ELOOP; accept either, since the security contract is
        // "no escape", and the fallback is deliberately stricter.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("real")).expect("mkdir real");
        std::fs::write(root.join("real/data"), b"in-tree").expect("write");
        symlink("real", root.join("link")).expect("symlink");

        match open_source_confined(&root, Path::new("link/data"), false) {
            Ok(file) => assert_eq!(read_to_string(file), "in-tree"),
            Err(error) => {
                // The portable walk refuses the symlinked directory component:
                // ELOOP on Linux, ENOTDIR on macOS/BSD (O_DIRECTORY evaluated
                // before O_NOFOLLOW).
                let code = error.raw_os_error();
                assert!(
                    code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
                    "fallback walk must refuse the symlink component, got: {error}"
                );
            }
        }
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

        let err = open_source_confined(&root, Path::new("escape/secret"), false)
            .expect_err("escape must be refused");
        // openat2 rejects the escape with EXDEV; the portable walk rejects the
        // symlinked directory component with ELOOP (Linux) or ENOTDIR
        // (macOS/BSD). Any of the three proves confinement.
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::EXDEV) || code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "expected EXDEV, ELOOP, or ENOTDIR for an escape, got: {err}"
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

        let err = open_source_confined(&root, Path::new("leak"), false)
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
        let err = open_source_confined(&root, Path::new("/etc/passwd"), false)
            .expect_err("absolute relative path must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn rejects_dotdot_component() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let err = open_source_confined(&root, Path::new("../secret"), false)
            .expect_err("dotdot component must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn missing_file_reports_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let err = open_source_confined(&root, Path::new("nope"), false)
            .expect_err("missing file must fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
