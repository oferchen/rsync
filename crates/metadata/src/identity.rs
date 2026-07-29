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
//! # `--copy-as` / `do_as_root`
//!
//! [`switch_effective_ids`](crate::copy_as::switch_effective_ids) temporarily
//! changes the effective ids for the duration of a [`SwitchScope`]. While such
//! a scope is active the accessors fall back to a live lookup so a switched
//! identity is observed, preserving the exact pre-cache behaviour for that
//! opt-in path. Outside any scope - the common case - the cached startup value
//! is returned with no syscall.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Number of currently-active effective-id switch scopes. While non-zero the
/// accessors perform a live lookup instead of returning the cached value.
static SWITCH_DEPTH: AtomicUsize = AtomicUsize::new(0);

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

/// The process effective uid. Cached at first use and reused thereafter,
/// except while an effective-id [`SwitchScope`] is active (`--copy-as`), when a
/// live lookup is performed so the switched identity is observed.
#[must_use]
pub fn effective_uid() -> u32 {
    if SWITCH_DEPTH.load(Ordering::Acquire) > 0 {
        return live_euid();
    }
    *CACHED_EUID.get_or_init(live_euid)
}

/// The process effective gid. See [`effective_uid`] for the caching and
/// switch-scope semantics.
#[must_use]
pub fn effective_gid() -> u32 {
    if SWITCH_DEPTH.load(Ordering::Acquire) > 0 {
        return live_egid();
    }
    *CACHED_EGID.get_or_init(live_egid)
}

/// Whether the process effective uid is root (uid 0). The oc analog of
/// upstream's cached `am_root` (main.c:1766).
#[must_use]
pub fn is_root() -> bool {
    effective_uid() == 0
}

/// RAII marker that makes the identity accessors read live for its lifetime.
/// Held by the `--copy-as`/`do_as_root` guard so that a temporarily switched
/// effective identity is observed by the ownership gates, matching the
/// behaviour before the cache existed.
#[derive(Debug)]
pub struct SwitchScope {
    _private: (),
}

impl SwitchScope {
    /// Enters an effective-id switch scope. The accessors read live until the
    /// returned guard is dropped.
    #[must_use]
    pub fn enter() -> Self {
        SWITCH_DEPTH.fetch_add(1, Ordering::Release);
        Self { _private: () }
    }
}

impl Drop for SwitchScope {
    fn drop(&mut self) {
        SWITCH_DEPTH.fetch_sub(1, Ordering::Release);
    }
}
