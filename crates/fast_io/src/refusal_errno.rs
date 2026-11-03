//! The errno a confinement refusal is reported with.
//!
//! rsync 3.5.1 has no `openat2` path. Its portable confined walk refuses every
//! escape - a symlink leaving the anchor, a `..` climbing above it - with
//! `ELOOP`, daemons included. oc's Linux `openat2(RESOLVE_BENEATH)` fast path
//! and its own sandbox walk raise `EXDEV` for the same refusal, and `EXDEV`
//! stays the internal escape signal callers branch on. Only the printed errno
//! is upstream's.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/syscall.c:2920-2931` - the walk "returns ELOOP" for a climb
//!   above the anchor and for an absolute symlink target.

use std::io;

/// The error to print for `error` when it came from a confined `stat` or
/// `mkdir`: an escape refusal (`EXDEV`) becomes upstream's `ELOOP`; anything
/// else is `None`, meaning print `error` itself.
///
/// Neither call can fail with a genuine cross-device `EXDEV`, so the rewrite
/// never relabels a real one. Do not apply it to a `rename` or `link` failure,
/// where `EXDEV` means what it says.
#[must_use]
pub fn reported_refusal(error: &io::Error) -> Option<io::Error> {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::EXDEV) {
        return Some(io::Error::from_raw_os_error(libc::ELOOP));
    }
    #[cfg(not(unix))]
    let _ = error;
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The escape refusal prints as upstream's "Too many levels of symbolic
    /// links (40)".
    #[test]
    fn an_escape_refusal_is_reported_as_eloop() {
        let shown = reported_refusal(&io::Error::from_raw_os_error(libc::EXDEV))
            .expect("EXDEV is rewritten");
        assert_eq!(shown.raw_os_error(), Some(libc::ELOOP));
    }

    /// Every other failure keeps its own errno, so a real EACCES or ENOMEM
    /// (the depth ceiling) is never relabelled.
    #[test]
    fn other_failures_are_reported_as_they_are() {
        for errno in [libc::EACCES, libc::ENOMEM, libc::ENOTDIR, libc::ELOOP] {
            assert!(reported_refusal(&io::Error::from_raw_os_error(errno)).is_none());
        }
        assert!(reported_refusal(&io::Error::other("no errno")).is_none());
    }
}
