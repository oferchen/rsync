//! Strict-resolution directory open for the SEC-1 dirfd sandbox.
//!
//! [`secure_open_dir`] returns a directory file descriptor that callers can
//! anchor subsequent `*at` syscalls against. The resolution policy refuses to
//! follow symlinks at the leaf on every Unix target, and on Linux 5.6+ kernels
//! it upgrades to `openat2(2)` with
//! `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` so that **any** symlink anywhere in
//! the path - not just at the leaf - and any `..` traversal outside the open
//! point are rejected with `EXDEV` / `ELOOP`.
//!
//! Windows callers do not need a sandbox dirfd because the NTFS handle-based
//! APIs sidestep path TOCTOU naturally (see the SEC-1.l audit). The Windows
//! stub therefore returns [`io::ErrorKind::Unsupported`] so callers can
//! fall through to the handle-based path.
//!
//! # Why two code paths on Linux
//!
//! `openat2(2)` landed in Linux 5.6 (March 2020). On older kernels the syscall
//! returns `ENOSYS`; we cache the first such result in a [`OnceLock`] and use
//! plain `open(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` thereafter. The plain
//! `open` path still rejects a symlink at the leaf - it just cannot reject
//! `..` traversal mid-path, which is the marginal extra confinement
//! `RESOLVE_BENEATH` gives us.
//!
//! # Single unsafe block
//!
//! Per `fast_io`'s unsafe-code policy, the libc invocations live behind one
//! `#[allow(unsafe_code)]` wrapper. The SAFETY argument is documented inline.

use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;

/// Open `path` as a directory file descriptor with strict resolution
/// semantics.
///
/// On Linux 5.6+ this uses `openat2` with
/// `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` for full path-component
/// confinement. On older Linux, macOS, and other Unix targets, this falls
/// back to `open(O_RDONLY | O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)`, which
/// only rejects a symlink at the leaf.
///
/// On Windows this returns `io::ErrorKind::Unsupported`; NTFS handle-based
/// APIs sidestep path TOCTOU without needing a sandbox dirfd.
///
/// # Errors
///
/// - `ELOOP` when the leaf is a symlink (plain `open` path) or any path
///   component is a symlink (`openat2` path with `RESOLVE_NO_SYMLINKS`).
/// - `ENOTDIR` when the path resolves to a non-directory.
/// - `EXDEV` (Linux only) when `openat2` rejects a `..` component that would
///   escape the open point under `RESOLVE_BENEATH`.
/// - `ENOENT` when the path does not exist.
/// - `EACCES` / `EPERM` per the usual `open(2)` semantics.
/// - `io::ErrorKind::Unsupported` on Windows.
pub fn secure_open_dir(path: &Path) -> io::Result<OwnedFd> {
    imp::secure_open_dir(path)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    pub(super) fn secure_open_dir(path: &Path) -> io::Result<OwnedFd> {
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains interior null byte",
            )
        })?;

        #[cfg(target_os = "linux")]
        {
            if let Some(fd) = linux::try_openat2(&c_path)? {
                return Ok(fd);
            }
        }

        open_nofollow(&c_path)
    }

    /// Plain `open(O_RDONLY | O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` fallback.
    ///
    /// Rejects a symlink at the leaf with `ELOOP`. Does not constrain
    /// mid-path components; callers that need that must run on a Linux 5.6+
    /// kernel where the `openat2` upgrade in [`secure_open_dir`] takes over.
    fn open_nofollow(c_path: &CString) -> io::Result<OwnedFd> {
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC;

        // SAFETY: `c_path` is a valid NUL-terminated C string borrowed for
        // the duration of the call. `libc::open` is a thread-safe syscall
        // wrapper that returns either a fresh, owned file descriptor or -1
        // with `errno` set. We immediately transfer ownership of any
        // non-negative fd to `OwnedFd::from_raw_fd`, which assumes exclusive
        // ownership and closes the fd on drop. No aliasing or use-after-free
        // is possible because we do not retain the raw fd anywhere else.
        #[allow(unsafe_code)]
        let raw = unsafe { libc::open(c_path.as_ptr(), flags) };

        if raw < 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `raw` is a non-negative fd just returned by `open(2)`
        // with `O_CLOEXEC`. We have not duplicated or leaked it; this is
        // the sole owner.
        #[allow(unsafe_code)]
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(fd)
    }

    #[cfg(target_os = "linux")]
    pub(super) mod linux {
        use super::*;
        use std::sync::OnceLock;

        /// Cached `openat2` availability probe.
        ///
        /// `None` until the first call; `Some(true)` once we have observed a
        /// successful `openat2` invocation; `Some(false)` once `ENOSYS` has
        /// come back. Subsequent calls skip the syscall when this is
        /// `Some(false)`.
        static OPENAT2_AVAILABLE: OnceLock<bool> = OnceLock::new();

        /// Attempts an `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`.
        ///
        /// Returns:
        /// - `Ok(Some(fd))` when the call succeeded - this is the strict
        ///   confinement path.
        /// - `Ok(None)` when the kernel does not support `openat2` (cached
        ///   for the remainder of the process lifetime), signalling the
        ///   caller to fall back to plain `open(O_NOFOLLOW)`.
        /// - `Err(_)` for any other failure, including the strict-resolution
        ///   refusals (`ELOOP`, `EXDEV`) that we want callers to see.
        pub(super) fn try_openat2(c_path: &CString) -> io::Result<Option<OwnedFd>> {
            if let Some(false) = OPENAT2_AVAILABLE.get().copied() {
                return Ok(None);
            }

            let how = libc::open_how {
                flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    as u64,
                mode: 0,
                resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS,
            };

            // SAFETY: `c_path` is a valid NUL-terminated C string borrowed
            // for the duration of the call. `how` is a fully-initialised
            // `open_how` whose address and `size_of::<open_how>()` we hand
            // to the kernel as required by `openat2(2)`. The syscall does
            // not retain the pointer past return. A non-negative return
            // value is a fresh, owned fd with `O_CLOEXEC` set; we
            // immediately transfer ownership to `OwnedFd::from_raw_fd`,
            // which is the sole owner thereafter.
            #[allow(unsafe_code)]
            let raw = unsafe {
                libc::syscall(
                    libc::SYS_openat2,
                    libc::AT_FDCWD,
                    c_path.as_ptr(),
                    &how as *const libc::open_how,
                    std::mem::size_of::<libc::open_how>(),
                )
            };

            if raw >= 0 {
                let _ = OPENAT2_AVAILABLE.set(true);
                // SAFETY: `raw` is a non-negative fd just returned by
                // `openat2(2)` with `O_CLOEXEC`. We have not duplicated or
                // leaked it.
                #[allow(unsafe_code)]
                let fd = unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) };
                return Ok(Some(fd));
            }

            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOSYS) {
                let _ = OPENAT2_AVAILABLE.set(false);
                return Ok(None);
            }
            Err(err)
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    pub(super) fn secure_open_dir(_path: &Path) -> io::Result<OwnedFd> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "secure_open_dir not implemented on Windows",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use tempfile::tempdir;

    #[test]
    fn opens_real_directory() {
        let dir = tempdir().expect("tempdir");

        #[cfg(unix)]
        {
            let fd = secure_open_dir(dir.path()).expect("open dir");
            assert!(fd.as_raw_fd() >= 0);
        }

        #[cfg(windows)]
        {
            let err = secure_open_dir(dir.path()).expect_err("unsupported on windows");
            assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_leaf() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::create_dir(&target).expect("create target dir");
        let link = dir.path().join("link");
        symlink(&target, &link).expect("create symlink");

        let err = secure_open_dir(&link).expect_err("symlink leaf must be rejected");
        // Linux + `openat2` (`RESOLVE_NO_SYMLINKS`) returns `ELOOP`.
        // Linux + plain `open(O_NOFOLLOW | O_DIRECTORY)` also returns `ELOOP`
        // because `O_NOFOLLOW` is evaluated first. macOS / BSD evaluate
        // `O_DIRECTORY` first, so they return `ENOTDIR` for the same input.
        // Either errno proves the symlink was refused, which is what the
        // SEC-1 sandbox needs from the leaf check.
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "expected ELOOP or ENOTDIR for symlink leaf, got: {err}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn openat2_rejects_dotdot_traversal_when_available() {
        let dir = tempdir().expect("tempdir");
        let child = dir.path().join("child");
        std::fs::create_dir(&child).expect("create child");

        // Probe whether the running kernel supports `openat2`. If not,
        // skip - the plain `open(O_NOFOLLOW)` path cannot police mid-path
        // `..` components, and that gap is acceptable on pre-5.6 kernels.
        if !openat2_available() {
            eprintln!("skipping: openat2 unavailable on this kernel");
            return;
        }

        // `child/../child` would resolve fine under plain `open`, but
        // `RESOLVE_BENEATH` rejects any `..` that traverses above the open
        // point, which `AT_FDCWD` anchors at the current working directory.
        // Use an absolute path with a `..` in the middle so the kernel
        // sees the traversal.
        let escaping = child.join("..").join("child");
        let err = secure_open_dir(&escaping)
            .expect_err("openat2 must reject .. traversal under RESOLVE_BENEATH");
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::EXDEV) || code == Some(libc::ELOOP),
            "expected EXDEV or ELOOP, got: {err}"
        );
    }

    #[cfg(target_os = "linux")]
    fn openat2_available() -> bool {
        use std::ffi::CString;
        let dot = CString::new(".").expect("CString");
        let how = libc::open_how {
            flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
            mode: 0,
            resolve: 0,
        };
        // SAFETY: `dot` is a NUL-terminated C string; `how` is a
        // fully-initialised `open_how`; we hand the kernel the matching
        // struct size. A non-negative return is an owned fd we
        // immediately close via `libc::close`.
        #[allow(unsafe_code)]
        let raw = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                libc::AT_FDCWD,
                dot.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if raw >= 0 {
            // SAFETY: `raw` is a non-negative fd just returned by
            // `openat2(2)`; we are its sole owner and close it here.
            #[allow(unsafe_code)]
            unsafe {
                libc::close(raw as libc::c_int);
            }
            true
        } else {
            io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS)
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_returns_unsupported() {
        let dir = tempdir().expect("tempdir");
        let err = secure_open_dir(dir.path()).expect_err("windows stub must error");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
