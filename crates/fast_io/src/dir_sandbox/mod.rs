//! Parent-dirfd carrier for the SEC-1 sandbox.
//!
//! [`DirSandbox`] is the runtime carrier the receiver pipeline threads
//! through every per-entry operation so that the SEC-1.f-j site-by-site
//! cutover from path-based syscalls to their `*at` siblings can resolve
//! relative names against a sandboxed parent file descriptor instead of
//! re-walking the path through the kernel. It implements the hybrid
//! "in-tree dirfd stack + side-cache" shape picked in
//! `docs/design/sec-1-b-dirfd-carrier.md` (section 1):
//!
//! 1. An **in-tree dirfd stack** ([`enter`](DirSandbox::enter) /
//!    [`exit`](DirSandbox::exit)) mirrors the receiver's depth-first
//!    descent. The top of the stack - or the root when the stack is empty -
//!    is the parent dirfd for the entry currently being applied, exposed
//!    via [`current_dirfd`](DirSandbox::current_dirfd) as a
//!    `BorrowedFd<'_>` so rayon workers can capture it by copy with zero
//!    synchronisation cost.
//! 2. A **side cache** of `Arc<OwnedFd>` keyed by canonical path
//!    ([`secondary`](DirSandbox::secondary)) covers the four
//!    cross-directory operands (`--backup-dir`, `--temp-dir`,
//!    `--link-dest`, `--copy-dest`, `--compare-dest`). The cache is a
//!    `DashMap` so the read-mostly lookup path stays lock-free; entries
//!    are inserted at receiver setup and never evicted.
//!
//! The module is `#[cfg(unix)]`. Windows uses NTFS handle-based ops per
//! the SEC-1.l audit and intentionally bypasses this carrier.
//!
//! # Resolution policy
//!
//! Every `*at` open issued by [`DirSandbox`] refuses to follow a symlink
//! at the leaf. On Linux 5.6+ kernels - detected via
//! [`openat2_supported`](crate::linux_capabilities::openat2_supported) -
//! the carrier upgrades to `openat2(2)` with
//! `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` so the kernel also rejects
//! any symlink along the path, any `..` traversal that would escape the
//! anchoring dirfd, and any magic-link resolution. Pairing
//! `RESOLVE_BENEATH` with a real anchoring dirfd is the supported
//! configuration (unlike the `AT_FDCWD` bootstrap in
//! [`secure_open_dir`](crate::secure_dir::secure_open_dir), which intentionally drops
//! `RESOLVE_BENEATH` because the cwd anchor is the wrong scope for
//! absolute paths).
//!
//! Older Linux and every other Unix target falls back to
//! `openat(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)`, which still rejects a
//! symlink at the leaf but cannot reject mid-path `..` traversal.
//!
//! # Threading model
//!
//! The stack is owned by the receiver thread and mutated only through
//! [`enter`](DirSandbox::enter) / [`exit`](DirSandbox::exit). Reads
//! through [`current_dirfd`](DirSandbox::current_dirfd) hand out
//! `BorrowedFd<'_>` values whose lifetime is bound to `&self`; rayon
//! workers capturing the borrow do so by copy because `BorrowedFd<'_>`
//! is `Copy + Send + Sync`. The side cache is a `DashMap` and supports
//! concurrent registration plus concurrent reads.
//!
//! # `unsafe` budget
//!
//! The only `unsafe` block in this module is the `openat2(2)` syscall
//! invocation, which mirrors the safety argument in
//! [`secure_open_dir`](crate::secure_dir::secure_open_dir). The
//! `openat(2)` fallback goes through `rustix`, which exposes a safe
//! interface; no `unsafe` is needed there.

use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;

use crate::linux_capabilities::openat2_supported;
use crate::secure_dir::secure_open_dir;

pub mod at_syscalls;

#[cfg(test)]
mod tests;

pub use at_syscalls::{
    AtMetadata, DirEntryView, EntryKind, LstatOutcome, ReadDirOutcome, UnlinkFlags, fchmodat,
    fchmodat_via_sandbox_or_fallback, fchownat, fchownat_via_sandbox_or_fallback, fstatat_nofollow,
    linkat, linkat_via_sandbox_or_fallback, lstat_via_sandbox_or_fallback, mkdirat,
    mkdirat_via_sandbox_or_fallback, openat, openat_via_sandbox_or_fallback,
    read_dir_via_sandbox_or_fallback, readlinkat, readlinkat_via_sandbox_or_fallback,
    recursive_unlinkat, recursive_unlinkat_via_sandbox_or_fallback, renameat,
    renameat_via_sandbox_or_fallback, secure_chmod_at, secure_chown_at, secure_utimes_at, symlinkat,
    symlinkat_via_sandbox_or_fallback, unlink_via_sandbox_or_fallback, unlinkat, utimensat,
    utimensat_via_sandbox_or_fallback,
};

/// Parent-dirfd carrier threaded through the receiver pipeline.
///
/// See the [module-level documentation](self) for the design rationale,
/// resolution policy, and threading model.
#[derive(Debug)]
pub struct DirSandbox {
    /// Root of the destination tree, opened once at receiver setup with
    /// [`secure_open_dir`].
    ///
    /// Held in an [`Arc`] so worker tasks that outlive a single
    /// [`enter`](Self::enter) / [`exit`](Self::exit) round can clone the
    /// handle cheaply. The root is the resolution scope for
    /// `RESOLVE_BENEATH` and the fallback when [`current_dirfd`] is
    /// called on an empty stack.
    ///
    /// [`current_dirfd`]: Self::current_dirfd
    root: Arc<OwnedFd>,
    /// In-tree dirfd stack. The top frame's fd is the parent dirfd for
    /// the entry currently being applied; an empty stack means the
    /// receiver is operating directly on the root.
    stack: Vec<DirFrame>,
    /// Side cache of secondary operand roots keyed by the absolute path
    /// the caller registered.
    ///
    /// Holds `Arc<OwnedFd>` so callers can keep a clone alive for the
    /// duration of a syscall without contending on a mutex. The map is
    /// sized by the number of CLI operands (typically <= 4) and is never
    /// pruned during a session.
    secondaries: DashMap<PathBuf, Arc<OwnedFd>>,
}

/// One frame of the in-tree descent stack.
#[derive(Debug)]
struct DirFrame {
    /// Leaf name of the directory, retained for diagnostics and so
    /// future SEC-1 work can reconstruct the relative path from the
    /// stack without re-walking the kernel.
    #[allow(dead_code)]
    leaf: std::ffi::OsString,
    /// Open `OwnedFd` for the directory.
    fd: OwnedFd,
}

impl DirSandbox {
    /// Open `root` and seed an empty descent stack.
    ///
    /// The root is opened through [`secure_open_dir`] so the bootstrap
    /// open refuses a symlink at the leaf (and, on Linux 5.6+, refuses
    /// any symlink anywhere in the path via
    /// `openat2(RESOLVE_NO_SYMLINKS)`).
    ///
    /// # Errors
    ///
    /// Propagates any failure from [`secure_open_dir`], including:
    /// - `ENOENT` when `root` does not exist.
    /// - `ENOTDIR` when `root` resolves to a non-directory.
    /// - `ELOOP` when `root` is a symlink.
    /// - `EACCES` per `open(2)` semantics.
    pub fn open_root(root: &Path) -> io::Result<Self> {
        let fd = secure_open_dir(root)?;
        Ok(Self {
            root: Arc::new(fd),
            stack: Vec::new(),
            secondaries: DashMap::new(),
        })
    }

    /// Borrow the parent dirfd for the entry currently being applied.
    ///
    /// Returns the top of the descent stack when non-empty; otherwise
    /// returns the root. The returned [`BorrowedFd`] is `Copy + Send +
    /// Sync` and can be captured into rayon worker closures without
    /// synchronisation. Hot-path accessor: returns in `O(1)` and issues
    /// no syscalls.
    #[must_use]
    pub fn current_dirfd(&self) -> BorrowedFd<'_> {
        match self.stack.last() {
            Some(frame) => frame.fd.as_fd(),
            None => self.root.as_fd(),
        }
    }

    /// Borrow the root dirfd directly, ignoring any pushed frames.
    ///
    /// Used by callers that need to resolve a path against the sandbox
    /// root rather than the current descent position (for example when
    /// re-anchoring after a worker thread has popped its own frames).
    #[must_use]
    pub fn root_dirfd(&self) -> BorrowedFd<'_> {
        self.root.as_fd()
    }

    /// Clone the root handle as an [`Arc`] for callers that need to
    /// outlive `&self`.
    ///
    /// Cheap: increments an atomic refcount and returns. The cloned
    /// handle is read-only from the receiver's perspective.
    #[must_use]
    pub fn root_arc(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.root)
    }

    /// Push a frame for `child_name` by opening the subdirectory off
    /// the current parent dirfd.
    ///
    /// The open uses `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`
    /// on Linux 5.6+ and `openat(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)`
    /// elsewhere. Both variants refuse a symlink at the leaf; the
    /// `openat2` upgrade additionally refuses to descend across a
    /// symlink anywhere in `child_name` and refuses `..` escapes.
    ///
    /// # Errors
    ///
    /// - `ELOOP` when `child_name` is or descends through a symlink
    ///   (kernel error; varies by Unix flavour).
    /// - `ENOENT` when `child_name` does not exist beneath the current
    ///   parent dirfd.
    /// - `ENOTDIR` when `child_name` resolves to a non-directory.
    /// - `EXDEV` (Linux + `openat2` only) when `child_name` contains a
    ///   `..` that would escape the parent dirfd under `RESOLVE_BENEATH`.
    pub fn enter(&mut self, child_name: &OsStr) -> io::Result<()> {
        let parent = self.current_dirfd();
        let fd = openat_dir(parent, child_name)?;
        self.stack.push(DirFrame {
            leaf: child_name.to_os_string(),
            fd,
        });
        Ok(())
    }

    /// Pop the top frame from the descent stack.
    ///
    /// Calling this with an empty stack is a no-op; callers are
    /// responsible for balancing every [`enter`](Self::enter) with one
    /// [`exit`](Self::exit). The popped `OwnedFd` is dropped, which
    /// closes the descriptor.
    pub fn exit(&mut self) {
        self.stack.pop();
    }

    /// Returns the current descent depth (number of pushed frames).
    ///
    /// Diagnostic accessor used by tests and tracing. A depth of zero
    /// means [`current_dirfd`](Self::current_dirfd) yields the root.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Register or look up a secondary operand root.
    ///
    /// On first call for a given `path` the helper opens the directory
    /// through [`secure_open_dir`] and stores the resulting `OwnedFd`
    /// in the `DashMap` keyed by `path`. Subsequent calls return the
    /// cached [`Arc`] without issuing a syscall. The clone is cheap and
    /// hands the caller a shared owner whose [`BorrowedFd`] can be
    /// passed to two-dirfd syscalls (`renameat`, `linkat`) alongside
    /// the in-tree dirfd from [`current_dirfd`](Self::current_dirfd).
    ///
    /// The `path` is used verbatim as the cache key. Callers that need
    /// to deduplicate across path aliases (`/var/log` vs
    /// `/var/log/../log`) should canonicalise before calling; the
    /// helper deliberately does not canonicalise on the caller's
    /// behalf because canonicalisation issues its own syscall traffic
    /// and is rarely the right default.
    ///
    /// # Errors
    ///
    /// Propagates any failure from [`secure_open_dir`] on the first
    /// call for `path`. A cached entry never fails.
    pub fn secondary(&self, path: &Path) -> io::Result<Arc<OwnedFd>> {
        if let Some(entry) = self.secondaries.get(path) {
            return Ok(Arc::clone(entry.value()));
        }
        let fd = secure_open_dir(path)?;
        let arc = Arc::new(fd);
        // `entry().or_insert_with` would race with another writer that
        // already opened the same operand. Use `entry().or_insert` on
        // the prepared Arc and discard our open if we lost the race.
        let stored = self
            .secondaries
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::clone(&arc))
            .clone();
        Ok(stored)
    }

    /// Returns the number of secondary operand entries currently
    /// cached.
    ///
    /// Diagnostic accessor used by tests and tracing to confirm
    /// idempotency of [`secondary`](Self::secondary).
    #[must_use]
    pub fn secondary_count(&self) -> usize {
        self.secondaries.len()
    }

    /// `fstatat(AT_SYMLINK_NOFOLLOW)` anchored on the current dirfd.
    ///
    /// SEC-1.f convenience accessor: resolves `leaf` relative to the
    /// dirfd returned by [`current_dirfd`](Self::current_dirfd) and
    /// returns the kernel's view of that entry without following a
    /// terminal symlink. Callers that already hold a [`BorrowedFd`]
    /// from a different anchor (for example
    /// [`root_dirfd`](Self::root_dirfd) or
    /// [`secondary`](Self::secondary)) should call
    /// [`fstatat_nofollow`] directly to make the anchoring explicit.
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`fstatat_nofollow`] error verbatim.
    pub fn lstat_at(&self, leaf: &OsStr) -> io::Result<AtMetadata> {
        fstatat_nofollow(self.current_dirfd(), leaf)
    }

    /// `unlinkat(dirfd, leaf, flags)` anchored on the current dirfd.
    ///
    /// SEC-1.g convenience accessor: resolves `leaf` relative to the
    /// dirfd returned by [`current_dirfd`](Self::current_dirfd) and
    /// removes the entry without re-walking the path through the
    /// kernel. `unlinkat(2)` never follows a terminal symlink, so a
    /// TOCTOU swap on `leaf` cannot redirect the unlink to an
    /// attacker-chosen inode beneath a different parent.
    ///
    /// Callers that already hold a [`BorrowedFd`] from a different
    /// anchor (for example [`root_dirfd`](Self::root_dirfd) or
    /// [`secondary`](Self::secondary)) should call [`unlinkat`]
    /// directly to make the anchoring explicit.
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`unlinkat`] error verbatim. See
    /// [`unlinkat`] for the notable error cases.
    pub fn unlinkat_at(&self, leaf: &OsStr, flags: UnlinkFlags) -> io::Result<()> {
        unlinkat(self.current_dirfd(), leaf, flags)
    }
}

/// Open `child_name` as a directory off `parent_fd` using the strictest
/// resolution policy the running kernel supports.
///
/// On Linux 5.6+ this uses `openat2(2)` with
/// `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`; otherwise it falls back to
/// `openat(2)` with `O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC` via `rustix`.
fn openat_dir(parent_fd: BorrowedFd<'_>, child_name: &OsStr) -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    {
        if openat2_supported()
            && let Some(fd) = linux::openat2_beneath(parent_fd, child_name)?
        {
            return Ok(fd);
        }
    }
    // Suppress the unused-import warning on non-Linux Unix targets.
    let _ = openat2_supported;

    openat_nofollow(parent_fd, child_name)
}

/// `openat(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` fallback.
///
/// Issued through `rustix::fs::openat`, which is a thin, safe wrapper
/// over the raw syscall.
fn openat_nofollow(parent_fd: BorrowedFd<'_>, child_name: &OsStr) -> io::Result<OwnedFd> {
    use rustix::fs::{Mode, OFlags};

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(parent_fd, child_name, flags, Mode::empty())
        .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;
    Ok(fd)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    /// Issue an `openat2(2)` for `child_name` beneath `parent_fd` with
    /// `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`.
    ///
    /// Returns `Ok(Some(fd))` on success, `Ok(None)` only if the kernel
    /// reports `ENOSYS` (which the `openat2_supported` cache should
    /// already have ruled out, but we defend against the race where the
    /// probe ran in a seccomp profile that has since been relaxed),
    /// and `Err` for every other failure - including the deliberate
    /// strict-resolution refusals (`ELOOP`, `EXDEV`).
    pub(super) fn openat2_beneath(
        parent_fd: BorrowedFd<'_>,
        child_name: &OsStr,
    ) -> io::Result<Option<OwnedFd>> {
        let c_name = CString::new(child_name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "child name contains interior null byte",
            )
        })?;

        // SAFETY: this single block performs three FFI touches:
        //
        // 1. `std::mem::zeroed::<open_how>()` - `libc::open_how` is
        //    `#[non_exhaustive]`, so a struct expression is unavailable.
        //    The type is `repr(C)` with integer-only fields, and an all-zero
        //    bit pattern is the documented "no constraint" value for every
        //    `openat2(2)` knob.
        //
        // 2. `libc::syscall(SYS_openat2, parent_fd, c_name, &how, size)` -
        //    `parent_fd.as_raw_fd()` is a live, borrowed fd whose lifetime
        //    is bound to `parent_fd: BorrowedFd<'_>` and outlives the
        //    syscall. `c_name` is a valid NUL-terminated C string borrowed
        //    for the duration of the call. `how` is a fully-initialised
        //    `open_how` whose address and `size_of::<open_how>()` we hand
        //    to the kernel as required by the syscall ABI. The kernel does
        //    not retain any of the pointers past return. A non-negative
        //    return value is a fresh, owned fd with `O_CLOEXEC` set.
        //
        // 3. `OwnedFd::from_raw_fd(raw)` - takes exclusive ownership of
        //    the fd just returned. We do not duplicate, leak, or alias
        //    the raw value anywhere else.
        #[allow(unsafe_code)]
        let raw = unsafe {
            let mut how: libc::open_how = std::mem::zeroed();
            how.flags =
                (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64;
            how.mode = 0;
            how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS;

            libc::syscall(
                libc::SYS_openat2,
                parent_fd.as_raw_fd(),
                c_name.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };

        if raw >= 0 {
            // SAFETY: `raw` is a non-negative fd just returned by
            // `openat2(2)` with `O_CLOEXEC`. We have not duplicated or
            // leaked it; this is the sole owner.
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
