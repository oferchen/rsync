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
use std::sync::atomic::{AtomicBool, Ordering};

use dashmap::DashMap;

use crate::linux_capabilities::openat2_supported;
use crate::secure_dir::secure_open_dir;

pub mod at_syscalls;

#[cfg(test)]
mod tests;

pub use at_syscalls::{
    AtMetadata, CloneAttempt, DirEntryView, EntryKind, LstatOutcome, ReadDirOutcome, UnlinkFlags,
    UnlinkResidue, confined_clone_file, confined_create_new, confined_link_anonymous,
    confined_rename, fchmodat, fchmodat_via_sandbox_or_fallback, fchownat,
    fchownat_via_sandbox_or_fallback, fstatat_nofollow, linkat, linkat_via_sandbox_or_fallback,
    lstat_via_sandbox_or_fallback, mkdirat, mkdirat_via_sandbox_or_fallback, openat,
    openat_via_sandbox_or_fallback, read_dir_via_sandbox_or_fallback, readlinkat,
    readlinkat_via_sandbox_or_fallback, recursive_unlinkat,
    recursive_unlinkat_via_sandbox_or_fallback, renameat, renameat_via_sandbox_or_fallback,
    secure_chmod_at, secure_chown_at, secure_utimes_at, symlinkat,
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
    /// Open a receiver destination root, splitting operator trust from peer
    /// trust.
    ///
    /// `anchor` is operator-supplied - a daemon module root from the config
    /// file, or the destination operand of a local/remote-shell run. It is
    /// opened with ordinary resolution via [`open_trusted_dir`](crate::open_trusted_dir), so an
    /// operator layout like `/srv -> /mnt/srv` resolves normally.
    ///
    /// `peer_tail` is the module-relative remainder the *peer* asked for. It
    /// is resolved component-by-component beneath the anchor descriptor, so
    /// it can neither climb above the anchor nor follow a planted symlink out
    /// of the served tree.
    ///
    /// Fusing the two into one absolute path and applying a single policy is
    /// what this method exists to prevent: confining the anchor breaks
    /// ordinary deployments, and trusting the tail is a module escape.
    ///
    /// # Upstream Reference
    ///
    /// - `syscall.c:85-90` `open_anchor_dirfd()` - plain `openat` for an
    ///   operator anchor.
    /// - `syscall.c:3189-3193` - "Absolute basedir: operator-trusted."
    /// - `syscall.c:2891` `ds_descend()` - the per-component walk that
    ///   confines the peer-supplied remainder.
    ///
    /// # Errors
    ///
    /// - Ordinary `open(2)` errors for `anchor` (`ENOENT`, `ENOTDIR`,
    ///   `EACCES`).
    /// - `EXDEV` when a `peer_tail` component would escape the anchor.
    /// - `ELOOP` when a `peer_tail` component is a symlink that cannot be
    ///   resolved beneath the anchor.
    #[cfg(unix)]
    pub fn open_dest_anchor(anchor: &Path, peer_tail: &Path) -> io::Result<Self> {
        Self::open_dest_anchor_with_policy(anchor, peer_tail, ConfinePolicy::operator_trusted())
    }

    /// [`open_dest_anchor`](Self::open_dest_anchor) with the trust policy made
    /// explicit.
    ///
    /// `policy` decides how components the peer named are resolved. The
    /// [`NoExclude`] arm implemented here keeps the kernel walk and is
    /// therefore behaviour-identical to the unpolicied entry point; the oracle
    /// arm replaces the mechanism and lands with its consumer in tasks
    /// 599/600.
    #[cfg(unix)]
    pub fn open_dest_anchor_with_policy(
        anchor: &Path,
        peer_tail: &Path,
        policy: ConfinePolicy<NoExclude>,
    ) -> io::Result<Self> {
        let ConfinePolicy { exclude } = policy;
        let mut fd = crate::secure_dir::open_trusted_dir(anchor)?;

        for component in peer_tail.components() {
            let name = match component {
                std::path::Component::Normal(name) => name,
                // `.` is a no-op; anything else (`..`, a root, a prefix) has
                // no business in a peer-supplied module-relative tail and is
                // refused rather than normalised away.
                std::path::Component::CurDir => continue,
                _ => {
                    return Err(io::Error::from_raw_os_error(libc::EXDEV));
                }
            };
            fd = openat_dir(fd.as_fd(), name)?;
            // Under `NoExclude` this is a constant `false`. A real oracle
            // cannot be honoured here at all - the kernel resolved the
            // component, so the only path available is the nominal one.
            if exclude.outside_confinement(Path::new(name)) {
                return Err(io::Error::from_raw_os_error(libc::ELOOP));
            }
        }

        Ok(Self {
            root: Arc::new(fd),
            stack: Vec::new(),
            secondaries: DashMap::new(),
        })
    }

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

/// Latch for the once-per-process descriptor-exhaustion hint.
static FD_EXHAUSTION_WARNED: AtomicBool = AtomicBool::new(false);

/// The hint text, byte-for-byte as upstream prints it.
///
/// upstream: syscall.c:2930-2931 - a bare `rprintf(FWARNING, ...)`, which
/// `rwrite()` routes to stderr verbatim (log.c:341). The
/// `rsync warning: ... (code N) at FILE(LINE) [role=version]` envelope is
/// **not** applied here; that wording is spelled out literally at its own
/// call site (log.c:956) and adding it would print text upstream does not.
const FD_EXHAUSTION_HINT: &str =
    "out of file descriptors resolving a deep path; raise the open-file limit (e.g. `ulimit -n`)";

/// Decide whether `err` is the first descriptor-exhaustion failure seen.
///
/// Returns `true` exactly once per `warned` latch, and only for `EMFILE`
/// (per-process limit) or `ENFILE` (system-wide limit). `swap` makes the
/// claim atomic, so concurrent rayon workers hitting the ceiling together
/// still produce a single line.
///
/// The latch is a parameter rather than a direct read of
/// [`FD_EXHAUSTION_WARNED`] so tests can exercise the first-time arm
/// without the process-global static coupling them to each other.
fn should_warn_fd_exhaustion(err: &io::Error, warned: &AtomicBool) -> bool {
    use rustix::io::Errno;

    let exhausted = err.raw_os_error().is_some_and(|raw| {
        raw == Errno::MFILE.raw_os_error() || raw == Errno::NFILE.raw_os_error()
    });

    exhausted && !warned.swap(true, Ordering::Relaxed)
}

/// Descend one component beneath `parent_fd`, following an in-tree symlink
/// and refusing an escape.
///
/// This is the single beneath-semantics primitive. Both the per-entry
/// descent and the peer-tail walk under an operator anchor go through it, so
/// there is one implementation of one upstream rule.
///
/// The policy is upstream's `ds_descend()` (`syscall.c:2891`): a relative
/// in-tree symlink target is spliced back into the walk, while an absolute
/// target or a climb above the anchor is refused. `RESOLVE_BENEATH` gives
/// exactly that, and `RESOLVE_NO_MAGICLINKS` blocks the `/proc` magic-link
/// detour. It is the same mask the peer-path parent anchor already uses
/// (`at_syscalls/nested.rs:201`), whose rationale this now shares rather
/// than contradicts.
///
/// `RESOLVE_NO_SYMLINKS` is deliberately *not* set. It refuses every symlink
/// component, including the in-tree ones upstream resolves, which turns a
/// symlinked subdirectory inside the destination tree into a hard failure.
///
/// Where `openat2(2)` is unavailable the walk degrades to `O_NOFOLLOW` per
/// component, which refuses in-tree symlinks the kernel path would follow.
/// That is stricter than upstream and is the portable-fallback gap tracked
/// for the rest of the sandbox.
///
/// # Descriptor exhaustion
///
/// The carrier holds one dirfd per live path component, so a deep descent
/// can exhaust the open-file limit where a single path-based `open()`
/// would not. Upstream prints a one-shot hint on `EMFILE`/`ENFILE` for
/// exactly this reason (upstream: syscall.c:2924-2936); without it the
/// bare "Too many open files" is opaque about which limit to raise.
///
/// Upstream brackets its `rprintf` with `int e = errno; ... errno = e;`
/// because `rprintf` can itself clobber the global `errno`. Rust has no
/// such hazard here: the failure is an owned [`io::Error`] moved through
/// this function, so nothing the emit does can alter what the caller
/// observes. The guarantee is pinned by test rather than by copying a
/// save/restore dance that would be a no-op.
fn openat_dir(parent_fd: BorrowedFd<'_>, child_name: &OsStr) -> io::Result<OwnedFd> {
    let result = openat_dir_strict(parent_fd, child_name);

    if let Err(err) = &result
        && should_warn_fd_exhaustion(err, &FD_EXHAUSTION_WARNED)
    {
        eprintln!("{FD_EXHAUSTION_HINT}");
    }

    result
}

/// The resolution itself, with no diagnostics attached.
fn openat_dir_strict(parent_fd: BorrowedFd<'_>, child_name: &OsStr) -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    {
        if openat2_supported()
            && let Some(fd) = linux::openat2_beneath(
                parent_fd,
                child_name,
                libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS,
            )?
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
        resolve: u64,
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
            // `O_NOFOLLOW` is deliberately absent. It refuses a symlink at the
            // final component, and every caller here passes a single component,
            // so the leaf *is* the whole path - it would refuse the in-tree
            // symlinks `RESOLVE_BENEATH` is here to let through, and it does so
            // before any `resolve` flag is consulted. Confinement is the
            // caller's `resolve` mask; an escape still fails with `EXDEV`.
            how.flags = (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64;
            how.mode = 0;
            how.resolve = resolve;

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

/// Consulted per descended component when a walk is confined to a module.
///
/// Only the manual per-component resolver can honour this: `openat2` resolves
/// symlinks in the kernel, so a `RESOLVE_BENEATH` walk never learns where a
/// symlink landed and could only offer the *nominal* path - which is exactly
/// what a symlink defeats. See `docs/design/path-confinement-resolver-api.md`
/// section 4.4.
///
/// # Upstream Reference
///
/// - `syscall.c:2891-2965` `ds_descend()` - extends `ds.abspath` per component
///   and refuses when `abspath_outside_confinement()` says the resolved path
///   left the module.
pub trait ConfinementOracle {
    /// True when `abspath` has left the served tree.
    fn outside_confinement(&self, abspath: &Path) -> bool;
}

/// The oracle for an operator-trusted walk: there is nothing to exclude.
///
/// Zero-sized, so a `ConfinePolicy<NoExclude>` carries no state and the
/// exclude check compiles out entirely. This mirrors upstream leaving
/// `ds.abspath` unseeded for a non-daemon caller, where the comment at
/// `syscall.c:2989-2991` notes such callers "pay nothing".
#[derive(Debug, Clone, Copy, Default)]
pub struct NoExclude;

impl ConfinementOracle for NoExclude {
    fn outside_confinement(&self, _abspath: &Path) -> bool {
        false
    }
}

/// How a walk treats components it did not itself name.
///
/// The policy selects the *resolution mechanism*, not merely the call shape:
/// `NoExclude` keeps the kernel walk (`RESOLVE_BENEATH`), while a real oracle
/// requires the manual per-component resolver, because the exclude check is
/// unimplementable on top of `openat2`.
///
/// The type parameter arrives with the oracle arm (tasks 599/600). Today only
/// the `NoExclude` instantiation exists, so it is spelled concretely rather
/// than speculatively generalised.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConfinePolicy<O = NoExclude> {
    exclude: O,
}

impl ConfinePolicy<NoExclude> {
    /// The operator named every component; nothing is confined.
    #[must_use]
    pub const fn operator_trusted() -> Self {
        Self { exclude: NoExclude }
    }
}

/// Symlink-hop budget for one confined walk, spent cumulatively across every
/// component and every followed target rather than reset per component.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:2794` `SECURE_OPEN_MAXSYMLINKS`
/// - `rsync-3.5.0/syscall.c:2966` `ds_walk_path()` takes `hops` by pointer,
///   which is what makes one budget span the whole walk.
const SECURE_OPEN_MAXSYMLINKS: u32 = 40;

/// Ceiling on directory descriptors one walk holds open at once.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:2801` `DS_MAXDEPTH`
const DS_MAXDEPTH: usize = 1024;

impl<O: ConfinementOracle> ConfinePolicy<O> {
    /// Confine the walk, consulting `exclude` after every descended component.
    ///
    /// Selects the manual per-component resolver. That arm trades `openat2`'s
    /// atomic resolution for the ability to evaluate the exclude check at all -
    /// a documented decision, not an oversight; see
    /// `docs/design/path-confinement-resolver-api.md` section 4.4.
    pub fn confined(exclude: O) -> Self {
        Self { exclude }
    }
}

/// True when an `O_NOFOLLOW` open failed *because the leaf was a symlink*.
///
/// Platforms disagree on the errno, so upstream tests the whole set rather
/// than branching per OS. `ENOTDIR` is handled separately by the caller: it
/// is ambiguous, meaning either a symlink (where `O_DIRECTORY` was evaluated
/// first) or a genuine non-directory, and only the `readlink` probe can tell
/// them apart.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/rsync.h:438-441` `NOFOLLOW_HIT_SYMLINK()`
#[cfg(unix)]
fn nofollow_hit_symlink(err: &io::Error) -> bool {
    let Some(code) = err.raw_os_error() else {
        return false;
    };
    #[cfg(any(
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "freebsd"
    ))]
    if code == libc::EFTYPE {
        return true;
    }
    code == libc::ELOOP || code == libc::EMLINK
}

/// One confined walk in progress: upstream's `struct dirstack`.
///
/// `anchor` is the operator-trusted base. Unlike upstream, which borrows
/// `fds[0]` and must `dup()` it in `ds_take()`, this walk owns its anchor, so
/// the leaf can be handed back directly.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:2803-2811` `struct dirstack`
#[cfg(unix)]
struct ConfinedWalk<'oracle, O: ConfinementOracle> {
    anchor: OwnedFd,
    pushed: Vec<OwnedFd>,
    /// Absolute path of the current directory, maintained across descents so
    /// the oracle sees where a followed symlink actually landed. Empty when
    /// the anchor is relative, which disables the check exactly as upstream
    /// disables it for an unseeded `ds.abspath`.
    abspath: PathBuf,
    exclude: &'oracle O,
    /// Shared across the whole walk. See [`SECURE_OPEN_MAXSYMLINKS`].
    hops: u32,
}

#[cfg(unix)]
impl<O: ConfinementOracle> ConfinedWalk<'_, O> {
    fn cur(&self) -> std::os::fd::BorrowedFd<'_> {
        match self.pushed.last() {
            Some(fd) => fd.as_fd(),
            None => self.anchor.as_fd(),
        }
    }

    fn push(&mut self, fd: OwnedFd, name: &std::ffi::OsStr) -> io::Result<()> {
        if self.pushed.len() + 1 >= DS_MAXDEPTH {
            return Err(io::Error::from_raw_os_error(libc::ENOMEM));
        }
        self.pushed.push(fd);
        if !self.abspath.as_os_str().is_empty() {
            self.abspath.push(name);
        }
        Ok(())
    }

    /// `..`: pop to the pinned parent, refusing to rise above the anchor.
    ///
    /// This is a *movement*, not a name to open, which is why it is handled
    /// here rather than refused as a malformed component. Upstream reports
    /// `ELOOP` when the stack is already at the anchor.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/syscall.c:2896-2901`
    fn pop(&mut self) -> io::Result<()> {
        if self.pushed.pop().is_none() {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        if !self.abspath.as_os_str().is_empty() {
            self.abspath.pop();
        }
        Ok(())
    }

    /// Descend one component, following an in-tree directory symlink.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/syscall.c:2891-2965` `ds_descend()`
    fn descend(&mut self, part: &std::ffi::OsStr) -> io::Result<()> {
        if part == "." {
            return Ok(());
        }
        if part == ".." {
            return self.pop();
        }

        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        match crate::dir_sandbox::at_syscalls::openat(self.cur(), part, flags, 0) {
            Ok(dir) => {
                self.push(OwnedFd::from(dir), part)?;
                if !self.abspath.as_os_str().is_empty()
                    && self.exclude.outside_confinement(&self.abspath)
                {
                    return Err(io::Error::from_raw_os_error(libc::ELOOP));
                }
                Ok(())
            }
            Err(err) if nofollow_hit_symlink(&err) || err.raw_os_error() == Some(libc::ENOTDIR) => {
                self.follow_symlink(part, err)
            }
            Err(err) => Err(err),
        }
    }

    /// Splice a symlink's target into the walk on the same stack.
    ///
    /// The target is relative and may itself contain `..` or further symlinks,
    /// which is why it re-enters [`walk_relative`](Self::walk_relative) rather
    /// than being opened directly.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/syscall.c:2937-2961`
    fn follow_symlink(&mut self, part: &std::ffi::OsStr, open_err: io::Error) -> io::Result<()> {
        let target = match crate::dir_sandbox::at_syscalls::readlinkat(self.cur(), part) {
            Ok(target) => target,
            // EINVAL means "not a symlink", so the component really was a
            // non-directory and the open's own errno is the honest answer.
            Err(err) if err.raw_os_error() == Some(libc::EINVAL) => return Err(open_err),
            Err(err) => return Err(err),
        };

        // An absolute target is refused outright - it names a path the anchor
        // does not contain, and following it would leave the module even when
        // it happens to resolve back inside.
        if target.as_os_str().is_empty() || target.is_absolute() {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }

        if self.hops == 0 {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        self.hops -= 1;

        self.walk_relative(&target)
    }

    /// Walk every component of a relative path on the current stack.
    ///
    /// Splits on `/` and skips empty tokens, mirroring upstream's
    /// `strtok_r(path, "/")` rather than `Path::components()`, whose
    /// normalisation rules are not the ones this walk is specified against.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/syscall.c:2966-2977` `ds_walk_path()`
    fn walk_relative(&mut self, path: &Path) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;

        for part in path.as_os_str().as_bytes().split(|byte| *byte == b'/') {
            if part.is_empty() {
                continue;
            }
            self.descend(std::ffi::OsStr::from_bytes(part))?;
        }
        Ok(())
    }

    /// Hand back the directory the walk finished on.
    fn into_leaf(self) -> OwnedFd {
        let Self {
            anchor, mut pushed, ..
        } = self;
        pushed.pop().unwrap_or(anchor)
    }
}

impl DirSandbox {
    /// Open an operator-trusted `anchor`, then walk `peer_tail` beneath it
    /// component by component, consulting the policy's oracle at every step.
    ///
    /// This is the arm [`open_dest_anchor_with_policy`](Self::open_dest_anchor_with_policy)
    /// cannot provide. `RESOLVE_BENEATH` resolves symlinks inside the kernel,
    /// so a confined caller never learns where one landed and could only test
    /// the nominal path - which is precisely what a symlink defeats.
    ///
    /// Relative in-tree symlinks are followed, absolute targets and climbs
    /// above the anchor are refused, and the symlink-hop budget is shared
    /// across the whole walk.
    ///
    /// # Errors
    ///
    /// - `ELOOP` - a refused symlink (absolute, empty, or landing outside the
    ///   confinement), a `..` that would rise above the anchor, or an
    ///   exhausted hop budget.
    /// - `ENOMEM` - the path is deeper than [`DS_MAXDEPTH`].
    /// - Otherwise the underlying `openat`/`readlinkat` errno.
    #[cfg(unix)]
    pub fn open_dest_anchor_confined<O: ConfinementOracle>(
        anchor: &Path,
        peer_tail: &Path,
        policy: ConfinePolicy<O>,
    ) -> io::Result<Self> {
        let ConfinePolicy { exclude } = policy;
        let anchor_fd = crate::secure_dir::open_trusted_dir(anchor)?;

        let mut walk = ConfinedWalk {
            anchor: anchor_fd,
            pushed: Vec::new(),
            // Seeded only for an absolute anchor. A relative one leaves the
            // tracker empty and the exclude check inert, mirroring upstream's
            // note that non-daemon callers "pay nothing"
            // (rsync-3.5.0/syscall.c:2989-2991).
            abspath: if anchor.is_absolute() {
                anchor.to_path_buf()
            } else {
                PathBuf::new()
            },
            exclude: &exclude,
            hops: SECURE_OPEN_MAXSYMLINKS,
        };

        walk.walk_relative(peer_tail)?;

        Ok(Self {
            root: Arc::new(walk.into_leaf()),
            stack: Vec::new(),
            secondaries: DashMap::new(),
        })
    }
}
