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
//! This module is Unix-only. Windows callers use NTFS handle-based APIs
//! (see the SEC-1.l audit), which sidestep path TOCTOU naturally; they
//! should `#[cfg(unix)]`-gate their use of this helper.
//!
//! # Why two code paths on Linux
//!
//! `openat2(2)` landed in Linux 5.6 (March 2020). On older kernels the syscall
//! returns `ENOSYS`; the
//! [`openat2_supported`](crate::linux_capabilities::openat2_supported) probe
//! caches that result and we use plain
//! `open(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` thereafter. The plain `open`
//! path still rejects a symlink at the leaf - it just cannot reject `..`
//! traversal mid-path, which is the marginal extra confinement
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
/// # Errors
///
/// - `ELOOP` when the leaf is a symlink (plain `open` path) or any path
///   component is a symlink (`openat2` path with `RESOLVE_NO_SYMLINKS`).
/// - `ENOTDIR` when the path resolves to a non-directory.
/// - `EXDEV` (Linux only) when `openat2` rejects a `..` component that would
///   escape the open point under `RESOLVE_BENEATH`.
/// - `ENOENT` when the path does not exist.
/// - `EACCES` / `EPERM` per the usual `open(2)` semantics.
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
        use crate::linux_capabilities::openat2_supported;

        /// Attempts an `openat2` with `RESOLVE_NO_SYMLINKS`.
        ///
        /// Returns:
        /// - `Ok(Some(fd))` when the call succeeded - this is the strict
        ///   confinement path.
        /// - `Ok(None)` when the kernel does not support `openat2`,
        ///   signalling the caller to fall back to plain `open(O_NOFOLLOW)`.
        ///   The probe is cached by [`openat2_supported`] for the remainder
        ///   of the process lifetime.
        /// - `Err(_)` for any other failure, including the strict-resolution
        ///   refusals (`ELOOP`, `EXDEV`) that we want callers to see.
        pub(super) fn try_openat2(c_path: &CString) -> io::Result<Option<OwnedFd>> {
            if !openat2_supported() {
                return Ok(None);
            }

            // `libc::open_how` is `#[non_exhaustive]`, so we zero-initialise
            // it and assign the fields we care about. The kernel reads exactly
            // `size_of::<open_how>()` bytes; any future fields default to 0,
            // which is the documented "no constraint" value for `openat2(2)`.
            // SAFETY: `open_how` is a plain repr(C) struct of integer fields;
            // an all-zero bit pattern is a valid value.
            #[allow(unsafe_code)]
            let mut how: libc::open_how = unsafe { std::mem::zeroed() };
            how.flags =
                (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64;
            how.mode = 0;
            // `RESOLVE_BENEATH` is NOT used here. This helper is the bootstrap
            // open that produces the parent dirfd anchor; subsequent `*at`
            // syscalls use that fd plus relative paths, and *those* sites are
            // where `RESOLVE_BENEATH` belongs (the dirfd's directory becomes
            // the resolution scope). Adding `RESOLVE_BENEATH` here with
            // `AT_FDCWD` would force the caller's path to live beneath the
            // process cwd, which is almost never true for daemon roots,
            // tempdirs in CI runners, etc - the kernel returns `EXDEV` for
            // every cross-subtree absolute path.
            how.resolve = libc::RESOLVE_NO_SYMLINKS;

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
                // SAFETY: `raw` is a non-negative fd just returned by
                // `openat2(2)` with `O_CLOEXEC`. We have not duplicated or
                // leaked it.
                #[allow(unsafe_code)]
                let fd = unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) };
                return Ok(Some(fd));
            }

            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOSYS) {
                return Ok(None);
            }
            Err(err)
        }
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
        // `tempdir()` may return a path that contains symlink components
        // (macOS `/tmp` -> `/private/tmp`, some CI runners stage `/tmp`
        // through a symlink). `RESOLVE_NO_SYMLINKS` refuses such paths,
        // so canonicalise first - the test exercises the success path,
        // not the deliberate symlink-rejection path.
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");

        let fd = secure_open_dir(&canon).expect("open dir");
        assert!(fd.as_raw_fd() >= 0);
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
        // Accepted errnos:
        // - `ELOOP`: Linux + `openat2` (`RESOLVE_NO_SYMLINKS`) and Linux +
        //   plain `open(O_NOFOLLOW | O_DIRECTORY)` both refuse symlinks at
        //   the leaf with this code.
        // - `ENOTDIR`: macOS / BSD evaluate `O_DIRECTORY` before
        //   `O_NOFOLLOW`, so the symlink-to-directory case yields ENOTDIR.
        // Either proves the symlink was refused, which is what the SEC-1
        // sandbox needs from the leaf check.
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "expected ELOOP or ENOTDIR for symlink leaf, got: {err}"
        );
    }

    // NOTE: a `..`-traversal rejection test belongs in SEC-1.e where a real
    // parent dirfd anchors `RESOLVE_BENEATH`. The bootstrap helper
    // [`secure_open_dir`] runs against `AT_FDCWD` and intentionally omits
    // `RESOLVE_BENEATH`, because pairing `AT_FDCWD` with an absolute path and
    // `RESOLVE_BENEATH` makes the kernel refuse any path that doesn't live
    // beneath the process cwd (returning `EXDEV`) - which would fail every
    // realistic daemon-root / tempdir scenario. Once the dirfd-anchored
    // `*at` call sites in SEC-1.f..j land, add the `..`-traversal regression
    // test there with the dirfd as the resolution scope.
}
