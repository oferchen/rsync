//! `*at` syscall helpers anchored on a [`DirSandbox`](super::DirSandbox).
//!
//! Wraps the bare libc entry points that have no safe equivalent in
//! `std::fs` (`fstatat`, `unlinkat`, etc.) and exposes them through a
//! typed surface the engine and transfer crates can consume without
//! taking on any `unsafe` of their own.
//!
//! This module carries every SEC-1 `*at` cutover:
//! - the lstat-class cutover for SEC-1.f
//!   (`fstatat(AT_SYMLINK_NOFOLLOW)`),
//! - the unlink-class cutover for SEC-1.g
//!   (`unlinkat(dirfd, name, 0 | AT_REMOVEDIR)`),
//! - the create-class cutover for SEC-1.h
//!   (`mkdirat`, `symlinkat`, `linkat`),
//! - the metadata-class cutover for SEC-1.i
//!   (`fchmodat`, `fchownat`, `utimensat`), and
//! - the rename-class cutover for SEC-1.j
//!   (`renameat`, optionally upgraded to `renameat2(RENAME_NOREPLACE)`
//!   on Linux 3.15+).
//!
//! Each helper takes a parent dirfd plus a single-component leaf so the
//! call cannot be redirected by a TOCTOU symlink swap between the
//! receiver's "decide to act" moment and the kernel reaching the inode.
//! The path-based `std::fs` / [`filetime`] fallbacks remain for
//! multi-component paths and the no-sandbox case so behaviour is
//! byte-identical for callers that have not yet plumbed a
//! [`DirSandbox`](super::DirSandbox).
//!
//! The implementation is split into one submodule per syscall family;
//! every public item is re-exported here so external callers can keep
//! using `at_syscalls::fstatat_nofollow()` etc. unchanged.

mod create;
mod lstat;
mod metadata;
mod metadata_ops;
mod nested;
mod open;
mod read_dir;
mod rename;
mod unlink;

#[cfg(test)]
mod tests;

pub use create::{
    linkat, linkat_via_sandbox_or_fallback, mkdirat, mkdirat_via_sandbox_or_fallback, symlinkat,
    symlinkat_via_sandbox_or_fallback,
};
pub use lstat::{LstatOutcome, lstat_via_sandbox_or_fallback};
pub use metadata::{AtMetadata, fstatat_nofollow};
pub use metadata_ops::{
    fchmodat, fchmodat_via_sandbox_or_fallback, fchownat, fchownat_via_sandbox_or_fallback,
    secure_chmod_at, secure_chown_at, secure_utimes_at, utimensat,
    utimensat_via_sandbox_or_fallback,
};
pub use open::{
    openat, openat_via_sandbox_or_fallback, readlinkat, readlinkat_via_sandbox_or_fallback,
};
pub use read_dir::{DirEntryView, EntryKind, ReadDirOutcome, read_dir_via_sandbox_or_fallback};
pub use rename::{renameat, renameat_via_sandbox_or_fallback};
pub use unlink::{
    UnlinkFlags, UnlinkResidue, recursive_unlinkat, recursive_unlinkat_via_sandbox_or_fallback,
    unlink_via_sandbox_or_fallback, unlinkat,
};

// Bring the leaf types the `#[cfg(test)]` module references via
// `use super::*` into module scope. Private `use`s are visible to the
// child `tests` module's glob import.
#[cfg(test)]
use std::ffi::OsStr;
#[cfg(test)]
use std::os::fd::BorrowedFd;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
use filetime::FileTime;

/// Return a pointer to the calling thread's `errno` cell.
///
/// libc exposes the thread-local cell under different names on
/// different targets: `__errno_location` on Linux, `__error` on
/// Apple/FreeBSD/Dragonfly, `__errno` on the remaining BSDs and on
/// Android. The shim forwards to whichever symbol the current target
/// provides so the caller can clear `errno` immediately before
/// invoking `readdir(3)` (which requires the clear-then-check idiom to
/// disambiguate end-of-stream from a real error).
///
/// Kept private to this module: callers should prefer
/// [`std::io::Error::last_os_error`] for syscalls that do not need the
/// pre-clear contract.
fn errno_location() -> *mut libc::c_int {
    // SAFETY: each libc accessor is documented as never failing and
    // returns a thread-local `*mut c_int` whose lifetime is the calling
    // thread. We only return the pointer here; dereferences live at
    // the call site behind their own `unsafe` block and SAFETY comment.
    #[allow(unsafe_code)]
    unsafe {
        #[cfg(any(target_os = "linux", target_os = "hurd", target_os = "redox"))]
        {
            libc::__errno_location()
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "dragonfly"
        ))]
        {
            libc::__error()
        }
        #[cfg(any(target_os = "netbsd", target_os = "openbsd", target_os = "android"))]
        {
            libc::__errno()
        }
        #[cfg(any(target_os = "illumos", target_os = "solaris"))]
        {
            libc::___errno()
        }
    }
}
