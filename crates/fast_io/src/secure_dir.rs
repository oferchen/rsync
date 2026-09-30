//! Strict-resolution directory open for the SEC-1 dirfd sandbox.
//!
//! [`secure_open_dir`] returns a directory file descriptor that callers can
//! anchor subsequent `*at` syscalls against. The resolution policy refuses to
//! follow symlinks at the leaf on every Unix target, and on Linux 5.6+ kernels
//! it upgrades to `openat2(2)` with `RESOLVE_NO_SYMLINKS`, so that **any**
//! symlink anywhere in the path - not just at the leaf - is rejected with
//! `ELOOP`.
//!
//! `RESOLVE_BENEATH` is deliberately NOT set; the reasoning is on `how.resolve`
//! in the `linux` submodule below. This helper *produces* an anchor, and
//! `AT_FDCWD` plus an absolute path would make `RESOLVE_BENEATH` refuse every
//! path outside the process cwd. Confining a path beneath an anchor belongs to
//! the `*at` walk that uses the fd this returns. A `..` component is therefore
//! **not** rejected here, and this function never returns `EXDEV`.
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
//! path still rejects a symlink at the leaf - it just cannot reject symlinks
//! in interior components, which is the extra confinement
//! `RESOLVE_NO_SYMLINKS` gives us.
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
/// On Linux 5.6+ this uses `openat2` with `RESOLVE_NO_SYMLINKS`, which
/// refuses a symlink in any path component. On older Linux, macOS, and other
/// Unix targets, this falls back to
/// `open(O_RDONLY | O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)`, which only rejects
/// a symlink at the leaf.
///
/// `RESOLVE_BENEATH` is **not** set, so a `..` component is not rejected and
/// this never returns `EXDEV`. That is deliberate: this call produces the
/// anchor, and `AT_FDCWD` with an absolute path would make `RESOLVE_BENEATH`
/// refuse every path outside the process cwd. Confinement beneath an anchor
/// belongs to the `*at` walk that uses the returned fd.
///
/// # Errors
///
/// - `ELOOP` when the leaf is a symlink (plain `open` path) or any path
///   component is a symlink (`openat2` path with `RESOLVE_NO_SYMLINKS`).
/// - `ENOTDIR` when the path resolves to a non-directory.
/// - `ENOENT` when the path does not exist.
/// - `EACCES` / `EPERM` per the usual `open(2)` semantics.
pub fn secure_open_dir(path: &Path) -> io::Result<OwnedFd> {
    imp::secure_open_dir(path)
}

/// Open an operator-supplied anchor directory with ordinary resolution.
///
/// This is the counterpart to [`secure_open_dir`] for a path the *operator*
/// chose - a daemon module root from the config file, or the destination
/// operand of a local/remote-shell invocation. Such a path is trusted: it is
/// not attacker-influenced, and refusing to resolve it through a symlink
/// breaks ordinary deployments (`/srv -> /mnt/srv`) for no security gain.
///
/// Confinement belongs on the *peer*-supplied remainder resolved beneath the
/// returned descriptor, not on the anchor itself.
///
/// # Upstream Reference
///
/// Upstream splits the two by who supplied the path:
///
/// - `syscall.c:102-107` `open_anchor_dirfd()` - a plain
///   `openat(AT_FDCWD, path, O_RDONLY | O_DIRECTORY)`, no `O_NOFOLLOW` and no
///   `resolve` flags.
/// - `syscall.c:3336-3340` - `secure_relative_open()` routes an absolute
///   `basedir` to that helper under the comment "Absolute basedir:
///   operator-trusted."
/// - `main.c:778` / `clientserver.c:993` - the receiver destination and the
///   daemon module root are entered with a plain `change_dir()`.
///
/// # Errors
///
/// Ordinary `open(2)` errors only: `ENOENT`, `ENOTDIR`, `EACCES`, `EPERM`.
/// Unlike [`secure_open_dir`] this never fails with `ELOOP` or `EXDEV`
/// merely because a component is a symlink.
///
/// # The pinned session root is duplicated, not re-resolved
///
/// When `path` names the root a daemon pinned by identity before its
/// privilege drop
/// ([`pin_session_root_fd`](crate::confinement::pin_session_root_fd)), this
/// hands back a duplicate of that descriptor instead of re-opening the
/// absolute path. Functionally identical - same inode, same ordinary symlink
/// resolution, already performed - but it does not re-traverse the root's
/// ancestors as the dropped uid, which `EACCES`es whenever the module sits
/// under a directory that uid cannot search (a 0700 home).
///
/// upstream: `syscall.c:102-107` `open_anchor_dirfd()` - `dup(module_dirfd)`
/// under exactly this condition, plain `openat(AT_FDCWD, ...)` otherwise.
pub fn open_trusted_dir(path: &Path) -> io::Result<OwnedFd> {
    if let Some(pinned) = crate::confinement::pinned_root_fd_for(path) {
        use std::os::fd::AsFd;
        return pinned.as_fd().try_clone_to_owned();
    }
    imp::open_trusted_dir(path)
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

    pub(super) fn open_trusted_dir(path: &Path) -> io::Result<OwnedFd> {
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains interior null byte",
            )
        })?;

        // upstream: rsync-3.5.1/syscall.c:102-107 open_anchor_dirfd() - no
        // `O_NOFOLLOW`, no `resolve` flags, and `directory_traverse_flags()`.
        // The anchor is operator-supplied, so ordinary symlink resolution
        // applies, and it is `*at()` authority only, so search permission is
        // enough.
        let flags = crate::owner_walk::traversal_dir_raw_flags();

        // SAFETY: `c_path` is a valid NUL-terminated C string borrowed for
        // the duration of the call. `libc::open` is a thread-safe syscall
        // wrapper returning either a fresh owned descriptor or -1 with
        // `errno` set. Ownership of any non-negative fd transfers
        // immediately to `OwnedFd::from_raw_fd`, which closes it on drop;
        // the raw value is not retained anywhere else, so no aliasing or
        // use-after-free is possible.
        #[allow(unsafe_code)]
        let raw = unsafe { libc::open(c_path.as_ptr(), flags) };

        if raw < 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `raw` is a non-negative fd just returned by `open(2)` with
        // `O_CLOEXEC`. It has not been duplicated or leaked; this is the sole
        // owner.
        #[allow(unsafe_code)]
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(fd)
    }

    /// Plain `open(O_RDONLY | O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` fallback.
    ///
    /// Rejects a symlink at the leaf with `ELOOP`. Does not constrain
    /// mid-path components; callers that need that must run on a Linux 5.6+
    /// kernel where the `openat2` upgrade in [`secure_open_dir`] takes over.
    fn open_nofollow(c_path: &CString) -> io::Result<OwnedFd> {
        let flags = crate::owner_walk::traversal_dir_raw_flags() | libc::O_NOFOLLOW;

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
            how.flags = (crate::owner_walk::traversal_dir_raw_flags() | libc::O_NOFOLLOW) as u64;
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
    #[cfg(target_os = "linux")]
    use std::os::fd::AsFd;
    use std::os::fd::AsRawFd;
    use tempfile::tempdir;

    /// A directory whose mode is `mode`, restored to 0755 on drop so the
    /// tempdir can be cleaned up.
    #[cfg(target_os = "linux")]
    struct ModeGuard(std::path::PathBuf);

    #[cfg(target_os = "linux")]
    impl ModeGuard {
        fn set(path: &Path, mode: u32) -> Self {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
            Self(path.to_path_buf())
        }

        /// Whether this process is actually denied reading the directory.
        /// Root (`CAP_DAC_READ_SEARCH`) is not, and then nothing here can be
        /// observed.
        fn read_is_denied(&self) -> bool {
            std::fs::read_dir(&self.0).is_err()
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// A destination below a searchable but unreadable (0111) parent must be
    /// reachable: the held anchor is traversal authority only, and traversal
    /// needs search permission, not read. Android exposes `/sdcard` this way.
    ///
    /// upstream: `rsync-3.5.1/syscall.c:83-92` `directory_traverse_flags()`,
    /// `:102-107` `open_anchor_dirfd()`; testsuite `search-only-destination`.
    #[cfg(target_os = "linux")]
    #[test]
    fn anchors_resolve_beneath_a_search_only_parent() {
        let dir = tempdir().expect("tempdir");
        let base = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let dest = base.join("search-only").join("dest");
        std::fs::create_dir_all(&dest).expect("mkdir");
        let guard = ModeGuard::set(&base.join("search-only"), 0o111);
        if !guard.read_is_denied() {
            return;
        }

        // The ownership walk opens every component itself, so resolving the
        // 0111 directory as the walk's final component is what a read-only
        // open cannot do: the receiver's destination-parent walk and
        // change_dir() both land there.
        crate::operator_open_dir(&base.join("search-only"))
            .expect("walked 0111 directory as the final component");
        let (parent, _) =
            crate::owner_trusted_parent(&dest).expect("walked parent that is itself 0111");
        drop(parent);
        let fd = secure_open_dir(&dest).expect("secure anchor beneath a 0111 parent");
        crate::dir_sandbox::at_syscalls::openat(
            fd.as_fd(),
            std::ffi::OsStr::new("created"),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o644,
        )
        .expect("create beneath the traversal anchor");
    }

    /// A write+search-only (0333) destination is a valid `*at()` authority:
    /// a known name can be created in it although it cannot be listed.
    ///
    /// upstream: testsuite `search-only-held-dirfd`, the mode 0333
    /// destination.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_write_search_only_directory_is_a_valid_anchor() {
        let dir = tempdir().expect("tempdir");
        let base = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let dest = base.join("write-only");
        std::fs::create_dir(&dest).expect("mkdir");
        let guard = ModeGuard::set(&dest, 0o333);
        if !guard.read_is_denied() {
            return;
        }

        let fd = open_trusted_dir(&dest).expect("0333 anchor");
        crate::dir_sandbox::at_syscalls::openat(
            fd.as_fd(),
            std::ffi::OsStr::new("incoming"),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o644,
        )
        .expect("create beneath a 0333 anchor");
    }

    /// Traversal authority is not read authority: listing a 0111 directory
    /// through its held anchor still fails with `EACCES`, as upstream's
    /// `secure_opendir()` does.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_search_only_directory_still_refuses_enumeration() {
        let dir = tempdir().expect("tempdir");
        let base = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let xonly = base.join("xonly");
        std::fs::create_dir(&xonly).expect("mkdir");
        let guard = ModeGuard::set(&xonly, 0o111);
        if !guard.read_is_denied() {
            return;
        }

        let fd = open_trusted_dir(&xonly).expect("0111 anchor");
        let error = crate::confined_readdir::read_dir_names_at(fd.as_fd())
            .expect_err("a search-only directory must not be enumerable");
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    }

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
