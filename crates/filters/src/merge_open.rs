//! Reading a per-directory filter merge file without following a planted symlink.
//!
//! During a recursive scan the sender opens a merge file from *each* scanned
//! directory, composing the path as `<scanned dir>/<merge pattern>`. Every one
//! of those directories may be attacker-controlled, so an unprivileged user can
//! plant the merge name as a symlink to any file the privileged rsync can read.
//! Its bytes are then parsed as filter rules, which both shapes what transfers
//! and discloses the target verbatim under `--debug=FILTER`.
//!
//! Upstream opens these through the same component walk it uses for every other
//! operator-named auxiliary file: follow a symlink only when it is owned by
//! uid 0 or our effective uid, refuse any other-uid one.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/exclude.c:1464` - `parse_filter_file()` opens the merge file.
//! - `rsync-3.5.0/exclude.c:811-814` - `push_local_filters()` calls it per
//!   scanned directory.
//! - `rsync-3.5.0/syscall.c:538` - `open_no_attacker_symlinks()`; the trust
//!   rule is at `syscall.c:406`.

use std::io;
use std::path::Path;

/// Read a per-directory merge file, refusing an attacker-owned symlink.
///
/// The platform seam lives here rather than at each call site. On non-Unix
/// targets the ownership walk has no meaning - there is no `st_uid` to trust -
/// and this degrades to a plain read, matching how the rest of the tree gates
/// `fast_io`'s operator-path helpers.
pub(crate) fn read_to_string(path: &Path) -> io::Result<String> {
    #[cfg(unix)]
    {
        fast_io::operator_read_to_string(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::read_to_string(path)
    }
}

/// Whether `error` is the ownership walk refusing an untrusted symlink.
///
/// Callers must treat this exactly as they treat a missing merge file. Upstream
/// invokes `parse_filter_file()` with `XFLG_ANCHORED2ABS` and *not*
/// `XFLG_FATAL_ERRORS`, so a merge file it cannot open is silently skipped: no
/// rule is added and the transfer proceeds. Surfacing the refusal as an error
/// would abort a transfer that upstream completes.
pub(crate) fn is_refusal(error: &io::Error) -> bool {
    fast_io::is_symlink_refusal(error)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The refusal predicate must recognise the walk's `ELOOP` and nothing else.
    ///
    /// If it widened to any error, a genuinely broken merge file would be
    /// silently ignored instead of reported; if it narrowed to nothing, a
    /// refused symlink would abort a transfer upstream completes.
    #[test]
    fn only_the_symlink_refusal_counts_as_a_refusal() {
        let refusal = io::Error::from_raw_os_error(libc::ELOOP);
        assert!(is_refusal(&refusal), "ELOOP is the walk's security refusal");

        for other in [libc::ENOENT, libc::EACCES, libc::ENOTDIR, libc::EIO] {
            let error = io::Error::from_raw_os_error(other);
            assert!(
                !is_refusal(&error),
                "errno {other} must not be mistaken for the refusal"
            );
        }
    }

    /// A merge file reached through a symlink the caller owns must still be
    /// read. The walk follows uid-0 and own-euid links by design, so this is
    /// the direction a refuse-all resolver would break - and an operator who
    /// keeps `.rsync-filter` behind a symlinked directory would lose their
    /// rules silently.
    #[test]
    fn a_self_owned_symlink_is_still_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("create temp dir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("create real dir");
        std::fs::write(real.join("rules"), b"- *.tmp\n").expect("write merge file");
        symlink(&real, dir.path().join("link")).expect("plant self-owned symlink");

        let contents =
            read_to_string(&dir.path().join("link").join("rules")).expect("read through symlink");

        assert_eq!(contents, "- *.tmp\n");
    }
}
