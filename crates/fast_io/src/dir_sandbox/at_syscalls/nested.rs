//! Nested-path parent anchoring for the `*_via_sandbox_or_fallback`
//! helpers.
//!
//! The single-component fast path in `single_component_leaf` pins the
//! leaf on the sandbox parent dirfd but leaves multi-component relative
//! paths (`a/b/leaf`) to re-resolve every interior component through the
//! ambient namespace on a path-based `std::fs` call. That re-resolution
//! is a TOCTOU hole: an attacker who swaps an *interior* directory
//! (`a/b`) for a symlink between the receiver's decide-to-act moment and
//! the syscall can redirect the leaf op outside the module root.
//!
//! [`anchor_parent`] closes that hole by opening the **parent**
//! directory of the leaf through `openat2(2)` with `RESOLVE_BENEATH`,
//! anchored on
//! [`current_dirfd`](crate::dir_sandbox::DirSandbox::current_dirfd), then
//! handing the caller a parent [`OwnedFd`] plus the leaf name so it can
//! issue the terminal `*at` op (`symlinkat`, `linkat`, `unlinkat`,
//! `renameat`, `fstatat`) with `O_NOFOLLOW` semantics against that fd.
//!
//! This mirrors upstream rsync's `secure_relative_open()` (see
//! `syscall.c:secure_relative_open_linux`, gated on
//! `am_daemon && !am_chrooted`), which opens the parent under
//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` and then performs
//! the leaf op against the resulting dirfd. `RESOLVE_BENEATH` (not
//! `RESOLVE_NO_SYMLINKS`) is the deliberate choice: legitimate in-tree
//! symlinks along the path stay resolvable, only escapes beneath the
//! anchor are refused with `EXDEV`.
//!
//! Off Linux there is no `openat2`, so the anchor is resolved by
//! [`DirSandbox::open_subdir_confined`](crate::dir_sandbox::DirSandbox::open_subdir_confined),
//! oc's port of upstream's own portable resolver `ds_descend()`
//! (`syscall.c:2891-2965`). It reaches the same decision on the same
//! inputs and refuses with `ELOOP` rather than `EXDEV`. The confinement
//! is the anchor descriptor itself, so it needs no confinement root -
//! which is why it works for a non-daemon client, where upstream also
//! leaves `confine_root` NULL (`syscall.c:142-143`).

use std::ffi::OsStr;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Component, Path};

/// Outcome of [`anchor_parent`].
///
/// Distinguishes the two cases the caller must handle differently so the
/// single-component fast path and the graceful-degradation contract stay
/// byte-identical to today's behaviour.
pub(super) enum ParentAnchor<'a> {
    /// The parent was resolved beneath the sandbox anchor. The caller
    /// issues the terminal `*at` op against `dirfd` with the leaf
    /// `name`.
    ///
    /// Built by `openat2(RESOLVE_BENEATH)` on Linux and by the portable
    /// per-component walk
    /// ([`DirSandbox::open_subdir_confined`](crate::dir_sandbox::DirSandbox::open_subdir_confined))
    /// elsewhere. Both admit a relative in-tree directory symlink and
    /// refuse anything that leaves the anchor.
    Anchored {
        /// Owned parent dirfd; kept alive by the caller for the
        /// duration of the leaf op.
        dirfd: OwnedFd,
        /// Leaf component to pass to the terminal `*at` op.
        name: &'a OsStr,
    },
    /// Nested anchoring does not apply (single-component path, no
    /// sandbox, path mismatch) or the Linux kernel lacks `openat2` /
    /// `RESOLVE_BENEATH`. The caller takes its current
    /// single-component-or-path-based behaviour unchanged.
    Fallback,
}

/// Resolve the parent directory of a multi-component relative path under
/// `RESOLVE_BENEATH`, anchored on the sandbox's current dirfd.
///
/// Returns:
/// - [`ParentAnchor::Anchored`] with the parent dirfd + leaf name when
///   `sandbox` is `Some`, `full_path == dest_dir.join(relative_path)`,
///   `relative_path` has two or more `Normal` components, and the parent
///   resolved beneath the root.
/// - [`ParentAnchor::Fallback`] when there is no sandbox, the path is a
///   single component, the reconstructed path does not match, the
///   relative path contains a non-`Normal` component (`..`, `.`,
///   absolute prefix), or - on Linux only - `openat2` /
///   `RESOLVE_BENEATH` is unavailable on the running kernel. In every
///   `Fallback` case the caller keeps its existing behaviour exactly.
///
/// # Errors
///
/// Propagates the resolver's error when the parent open reaches the
/// kernel and is refused for a *security* reason: `EXDEV` (a `..` or
/// symlink escape beneath the anchor, Linux) or `ELOOP` (the same
/// refusals off Linux), plus `ENOENT` (missing interior component) and
/// `ENOTDIR`. These are deliberate refusals - the caller must **not**
/// fall back to a path-based syscall on this error, because doing so
/// would re-open the TOCTOU window the anchor closes. Only `ENOSYS` /
/// `EINVAL` on the Linux resolve flags are folded into
/// [`ParentAnchor::Fallback`] (kernel lacks the capability).
pub(super) fn anchor_parent<'a>(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &'a Path,
    full_path: &Path,
) -> io::Result<ParentAnchor<'a>> {
    let Some(sandbox) = sandbox else {
        return Ok(ParentAnchor::Fallback);
    };
    // Every component must be a plain name. A `..`, `.`, or absolute
    // prefix means we cannot safely split into parent + leaf here, so we
    // defer to the caller's existing behaviour (path-based). Receiver
    // relative paths are flist-derived leaf names and never contain
    // these, so this is a defensive gate, not a hot-path branch.
    let mut names: Vec<&OsStr> = Vec::new();
    for comp in relative_path.components() {
        match comp {
            Component::Normal(name) => names.push(name),
            _ => return Ok(ParentAnchor::Fallback),
        }
    }
    // Single component (or empty) is the existing fast path; let the
    // caller handle it byte-identically.
    if names.len() < 2 {
        return Ok(ParentAnchor::Fallback);
    }
    // The absolute path the caller intends to act on must be exactly the
    // sandbox root joined with the relative path; otherwise the sandbox
    // dirfd is the wrong anchor and we must not act against it.
    if dest_dir.join(relative_path) != full_path {
        return Ok(ParentAnchor::Fallback);
    }

    let (leaf, parent_names) = names.split_last().expect("len >= 2 checked above");
    let parent_rel: std::path::PathBuf = parent_names.iter().collect();

    #[cfg(target_os = "linux")]
    {
        use crate::linux_capabilities::openat2_supported;
        if openat2_supported() {
            return match linux::openat2_parent_beneath(sandbox.current_dirfd(), &parent_rel) {
                Ok(Some(dirfd)) => Ok(ParentAnchor::Anchored { dirfd, name: leaf }),
                // ENOSYS / EINVAL: capability absent, degrade gracefully.
                Ok(None) => Ok(ParentAnchor::Fallback),
                // Deliberate refusal (EXDEV/ELOOP/ENOENT/ENOTDIR): the
                // op must fail, never silently re-resolve via a path.
                Err(err) => Err(err),
            };
        }
        Ok(ParentAnchor::Fallback)
    }

    #[cfg(not(target_os = "linux"))]
    {
        // No `openat2` here, but confinement does not require it: upstream's
        // own resolver is a portable per-component walk, and oc already ports
        // it as `DirSandbox::open_subdir_confined` (upstream `ds_descend`,
        // `syscall.c:2891-2965`). It admits and refuses exactly what
        // `RESOLVE_BENEATH` does - a relative in-tree directory symlink is
        // followed, an absolute target or a `..` above the anchor is refused -
        // so the two arms agree on policy and differ only in errno (`ELOOP`
        // here, `EXDEV` there).
        //
        // Returning `Fallback` instead sent the caller to
        // `ConfinedFallback::confined()`, whose ownership walk TRUSTS a
        // euid-owned symlink and whose escape check is inert without a
        // `SESSION_ROOT`. A non-daemon client pull has no confinement root, so
        // a peer-planted `--partial-dir` symlink was followed straight out of
        // the destination tree - the
        // `malicious-server-partial-basis-symlink-overwrite` cell.
        match sandbox.open_subdir_confined(&parent_rel) {
            Ok(dirfd) => Ok(ParentAnchor::Anchored { dirfd, name: leaf }),
            // A refusal must stay a refusal. Degrading to a path-based syscall
            // here would re-resolve every component through the ambient
            // namespace and undo the walk that just said no.
            Err(err) => Err(err),
        }
    }
}

/// Returns `true` when [`anchor_parent`] resolves a multi-component path's
/// parent beneath the sandbox root on this host, and `false` when it degrades
/// to the caller's path-based fallback.
///
/// This is the capability the `*_via_sandbox_or_fallback` family branches on,
/// and it is **not** the same question as
/// [`openat2_supported`](crate::openat2_supported):
///
/// - **Off Linux** anchoring is always available: the parent resolves through
///   [`DirSandbox::open_subdir_confined`](crate::dir_sandbox::DirSandbox::open_subdir_confined),
///   oc's port of upstream's portable `ds_descend()` (`syscall.c:2891-2965`),
///   which needs no kernel support.
/// - **On Linux** anchoring uses `openat2(RESOLVE_BENEATH)`, so it tracks
///   `openat2_supported()`. A kernel below 5.6 is the one state that degrades.
///
/// Ask this rather than re-deriving it from `openat2_supported()`. The two
/// predicates agreed until the portable arm landed, so a site still spelling
/// the old formula asserts the pre-change contract while looking correct.
#[must_use]
pub fn nested_parent_anchoring_supported() -> bool {
    !cfg!(target_os = "linux") || crate::linux_capabilities::openat2_supported()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// `openat2(parent_fd, parent_rel, RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`
    /// returning the opened parent directory fd.
    ///
    /// `RESOLVE_BENEATH` allows legitimate in-tree symlinks along
    /// `parent_rel` but refuses any component (symlink target or `..`)
    /// that would escape beneath `parent_fd`, matching upstream
    /// `secure_relative_open_linux`. `RESOLVE_NO_MAGICLINKS` refuses
    /// `/proc`-style magic links, also matching upstream.
    ///
    /// Returns `Ok(None)` only on `ENOSYS` (kernel lacks the syscall
    /// despite the probe) or `EINVAL` (resolve flags unsupported), so
    /// the caller can degrade to path-based resolution. Every other
    /// error is a deliberate refusal and is returned verbatim.
    pub(super) fn openat2_parent_beneath(
        parent_fd: BorrowedFd<'_>,
        parent_rel: &Path,
    ) -> io::Result<Option<OwnedFd>> {
        let c_rel = CString::new(parent_rel.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "parent path contains interior null byte",
            )
        })?;

        // SAFETY: mirrors `dir_sandbox::linux::openat2_beneath`.
        //
        // 1. `std::mem::zeroed::<open_how>()` - `libc::open_how` is
        //    `#[non_exhaustive]`; the all-zero pattern is the documented
        //    "no constraint" value for every field, which we then
        //    override explicitly.
        // 2. `libc::syscall(SYS_openat2, ...)` - `parent_fd.as_raw_fd()`
        //    is a live borrowed fd outliving the call; `c_rel` is a
        //    valid NUL-terminated C string borrowed for the call; `how`
        //    is fully initialised and handed to the kernel with its size
        //    per the ABI. The kernel retains no pointer past return. A
        //    non-negative return is a fresh owned fd with `O_CLOEXEC`.
        // 3. `OwnedFd::from_raw_fd(raw)` - sole owner of the returned
        //    fd; never aliased or leaked.
        #[allow(unsafe_code)]
        let raw = unsafe {
            let mut how: libc::open_how = std::mem::zeroed();
            how.flags = (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64;
            how.mode = 0;
            how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS;

            libc::syscall(
                libc::SYS_openat2,
                parent_fd.as_raw_fd(),
                c_rel.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };

        if raw >= 0 {
            // SAFETY: `raw` is a non-negative fd just returned by
            // `openat2(2)` with `O_CLOEXEC`; sole owner.
            #[allow(unsafe_code)]
            let fd = unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) };
            return Ok(Some(fd));
        }

        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ENOSYS) | Some(libc::EINVAL) => Ok(None),
            _ => Err(err),
        }
    }
}
