//! Cached process effective identity (euid/egid) for the metadata-apply hot
//! path.
//!
//! Upstream rsync reads the process identity exactly once at startup and
//! reuses it for every per-file ownership/permission decision:
//!
//! ```c
//! // main.c:1764-1766
//! our_uid = MY_UID();          // geteuid()
//! our_gid = MY_GID();          // getegid()
//! am_root = our_uid == ROOT_UID;
//! ```
//!
//! Every later `change_uid = am_root && ...` (rsync.c:526) test consults those
//! cached globals - upstream never calls `geteuid()`/`getegid()` per file. The
//! oc-rsync ownership gates previously called `nix::unistd::geteuid()` /
//! `getegid()` for every file, which an strace of a 30000-file local copy
//! showed as ~2 `geteuid` + 1 `getegid` per file (90000 syscalls upstream does
//! not make). This module caches the values once to match upstream.
//!
//! # fakeroot
//!
//! The lookups use libc (`geteuid`/`getegid`), NOT `rustix`'s raw syscall, so
//! that under `fakeroot`/`fakeroot-ng` (which intercept the libc symbols) the
//! *faked* root identity is observed - exactly as upstream's libc-based
//! `MY_UID()` sees it. `rustix` issues the raw syscall and would report the
//! real non-root euid, gating chowns away where upstream performs them.
//!
//! # `--copy-as`
//!
//! [`become_copy_as_user`](crate::copy_as::become_copy_as_user) performs a
//! permanent, irreversible privilege drop (`setgid`/`setgroups`/`setuid`) and
//! then calls [`set_process_identity`] with the new uid/gid. The accessors
//! consult that override first, so every subsequent ownership/permission gate
//! sees the dropped identity - even though the OnceLock cache may already hold
//! the pre-drop (root) value from an earlier call. This mirrors upstream, which
//! drops privilege once in `become_copy_as_user()` (main.c) before the first
//! file is processed and never re-elevates.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};

/// Overridden effective uid/gid installed by a permanent privilege drop
/// ([`set_process_identity`]). `-1` means "unset"; a non-negative value takes
/// precedence over the OnceLock cache so a post-drop identity is always
/// observed regardless of what the cache captured before the drop.
static OVERRIDE_UID: AtomicI64 = AtomicI64::new(-1);
static OVERRIDE_GID: AtomicI64 = AtomicI64::new(-1);

static CACHED_EUID: OnceLock<u32> = OnceLock::new();
static CACHED_EGID: OnceLock<u32> = OnceLock::new();

#[allow(unsafe_code)]
fn live_euid() -> u32 {
    // SAFETY: `geteuid` is a standard POSIX call with no arguments and no side
    // effects. libc (not rustix) so fakeroot's faked identity is honoured.
    unsafe { libc::geteuid() }
}

#[allow(unsafe_code)]
fn live_egid() -> u32 {
    // SAFETY: `getegid` is a standard POSIX call with no arguments and no side
    // effects. libc (not rustix) so fakeroot's faked identity is honoured.
    unsafe { libc::getegid() }
}

/// The process effective uid. A permanent-drop override ([`set_process_identity`])
/// wins when set (`--copy-as`); otherwise the value is cached at first use and
/// reused thereafter with no syscall, matching upstream's startup `our_uid`.
#[must_use]
pub fn effective_uid() -> u32 {
    let overridden = OVERRIDE_UID.load(Ordering::Acquire);
    if overridden >= 0 {
        return overridden as u32;
    }
    *CACHED_EUID.get_or_init(live_euid)
}

/// The process effective gid. See [`effective_uid`] for the override and
/// caching semantics.
#[must_use]
pub fn effective_gid() -> u32 {
    let overridden = OVERRIDE_GID.load(Ordering::Acquire);
    if overridden >= 0 {
        return overridden as u32;
    }
    *CACHED_EGID.get_or_init(live_egid)
}

/// Whether the process effective uid is root (uid 0). The oc analog of
/// upstream's cached `am_root` (main.c:1766).
#[must_use]
pub fn is_root() -> bool {
    effective_uid() == 0
}

/// Records the process's effective identity after a permanent privilege drop.
///
/// Called by [`become_copy_as_user`](crate::copy_as::become_copy_as_user) once
/// the `setgid`/`setgroups`/`setuid` sequence has completed. The stored uid/gid
/// take precedence over the OnceLock cache in [`effective_uid`]/[`effective_gid`],
/// so ownership gates observe the dropped identity even if the cache was primed
/// with the pre-drop (root) value earlier in the run. The drop is irreversible,
/// so there is no corresponding clear operation.
pub fn set_process_identity(uid: u32, gid: u32) {
    OVERRIDE_UID.store(i64::from(uid), Ordering::Release);
    OVERRIDE_GID.store(i64::from(gid), Ordering::Release);
}

#[cfg(all(test, unix))]
mod tests {
    use super::is_root;

    /// `am_root` must agree with the **libc** `geteuid`, never a raw syscall.
    ///
    /// The two disagree under `fakeroot`, which intercepts the libc symbol and
    /// reports uid 0 while the raw syscall still reports the real unprivileged
    /// uid. Upstream reads libc (`main.c:1764 our_uid = MY_UID()`), so a
    /// raw-syscall `am_root` refuses device creation and ownership changes in
    /// exactly the runs where upstream performs them - which is how the 3.5.0
    /// `devices` testsuite cell, whose non-root leg re-execs under fakeroot,
    /// observes the difference.
    #[test]
    #[allow(unsafe_code)]
    fn am_root_tracks_the_libc_effective_uid() {
        // SAFETY: `geteuid` is a POSIX accessor taking no arguments, with no
        // side effects and no failure mode.
        let libc_euid = unsafe { libc::geteuid() };
        assert_eq!(crate::am_root(), libc_euid == 0);
        assert_eq!(is_root(), libc_euid == 0);
    }
}
