//! One-shot diagnostics for POSIX metadata classes a non-Unix (Windows) target
//! cannot apply.
//!
//! Windows/NTFS has no POSIX uid/gid ownership and exposes only a single
//! read-only permission bit, so the receiver necessarily drops ownership
//! entirely and every mode bit except read-only. The drops themselves are
//! unavoidable POSIX->NTFS mapping gaps; performing them *silently* is the bug.
//! Upstream rsync never hides an attribute it cannot honor - it reports the
//! condition to the user with `rprintf(FWARNING, ...)` / `rsyserr(...)`, which
//! `log.c:rwrite()` routes to stderr, and continues the transfer. This module
//! mirrors that behaviour.
//!
//! The loss is total and uniform across every file in the transfer (it is a
//! property of the platform, not of any individual entry), so the correct
//! granularity is one warning per affected metadata *class*, emitted the first
//! time an apply is attempted - never once per file. Each warning fires only
//! when the user actually requested the class, so a plain copy that asked for
//! neither ownership nor permissions stays quiet.
//!
//! upstream: rsync.c:set_file_attrs() surfaces metadata it cannot set via
//! rprintf(FWARNING, ...); log.c:rwrite(FWARNING, ...) sends it to stderr.

#[cfg(any(not(unix), test))]
use crate::MetadataOptions;
#[cfg(any(not(unix), test))]
use std::sync::atomic::{AtomicBool, Ordering};

/// Warning body emitted once when a Windows target cannot apply requested
/// ownership. Kept as a named constant so its wording can be pinned in a test.
#[cfg(any(not(unix), test))]
pub(crate) const OWNERSHIP_WARNING: &str =
    "warning: file ownership (uid/gid) is not applied on Windows; ownership preservation skipped";

/// Warning body emitted once when a Windows target can preserve only the
/// read-only bit of the requested POSIX permissions.
#[cfg(any(not(unix), test))]
pub(crate) const PERMISSIONS_WARNING: &str = "warning: POSIX permission bits beyond read-only are not applied on Windows; only the read-only bit is preserved";

/// Reports whether the user requested any file-ownership preservation that a
/// Windows target cannot honor.
///
/// Mirrors the option set that drives `chown` and id-mapping on Unix: `-o`/`-g`
/// (owner/group), `--chown` (owner/group overrides), and `--usermap`/
/// `--groupmap` (id maps). Any of these implies the user expects uid/gid to be
/// applied, which Windows cannot do.
#[cfg(any(not(unix), test))]
#[must_use]
pub(crate) fn ownership_requested(options: &MetadataOptions) -> bool {
    options.owner()
        || options.group()
        || options.owner_override().is_some()
        || options.group_override().is_some()
        || options.user_mapping().is_some()
        || options.group_mapping().is_some()
}

/// Reports whether the user requested POSIX permission semantics beyond the one
/// bit Windows can map (read-only): `-p` (full mode), `--chmod`, or `-E`
/// (executability). When only the read-only bit would be affected there is no
/// loss to report.
#[cfg(any(not(unix), test))]
#[must_use]
pub(crate) fn permissions_beyond_readonly_requested(options: &MetadataOptions) -> bool {
    options.permissions() || options.chmod().is_some() || options.executability()
}

/// Runs `emit` at most once across every call sharing `already_warned`, and
/// only when `should_warn` is true.
///
/// Separated from the concrete Windows emitters (which are `#[cfg]`-compiled
/// out on Unix) so the once-per-class + request-gating contract can be
/// unit-tested on any host. `Ordering::Relaxed` is sufficient: the only shared
/// state is the boolean latch and the message carries no other data.
#[cfg(any(not(unix), test))]
fn warn_once_if(should_warn: bool, already_warned: &AtomicBool, emit: impl FnOnce()) {
    if should_warn && !already_warned.swap(true, Ordering::Relaxed) {
        emit();
    }
}

/// Emits [`OWNERSHIP_WARNING`] to stderr once per process when ownership was
/// requested but cannot be applied on this non-Unix target.
#[cfg(not(unix))]
pub(crate) fn warn_ownership_unsupported(options: &MetadataOptions) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    warn_once_if(ownership_requested(options), &WARNED, || {
        eprintln!("{OWNERSHIP_WARNING}");
    });
}

/// Emits [`PERMISSIONS_WARNING`] to stderr once per process when permission
/// bits beyond read-only were requested but cannot be applied on this non-Unix
/// target.
#[cfg(not(unix))]
pub(crate) fn warn_permissions_unsupported(options: &MetadataOptions) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    warn_once_if(
        permissions_beyond_readonly_requested(options),
        &WARNED,
        || {
            eprintln!("{PERMISSIONS_WARNING}");
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chmod::ChmodModifiers;
    #[cfg(unix)]
    use crate::{GroupMapping, UserMapping};
    use std::cell::Cell;

    /// A bare copy that requested neither ownership nor permission bits. The
    /// default `preserve_permissions` is `true`, so clear it to model a caller
    /// who asked for nothing this platform cannot honor.
    fn plain() -> MetadataOptions {
        MetadataOptions::new().preserve_permissions(false)
    }

    #[test]
    fn ownership_requested_tracks_each_owner_option() {
        assert!(!ownership_requested(&plain()));

        assert!(ownership_requested(&plain().preserve_owner(true)));
        assert!(ownership_requested(&plain().preserve_group(true)));
        assert!(ownership_requested(&plain().with_owner_override(Some(0))));
        assert!(ownership_requested(&plain().with_group_override(Some(0))));
        // --usermap/--groupmap can only be constructed on Unix; the Windows
        // mapping type is a deliberately never-constructible placeholder, so
        // user_mapping()/group_mapping() are always None there and these legs
        // of ownership_requested are only reachable on Unix.
        #[cfg(unix)]
        {
            assert!(ownership_requested(
                &plain().with_user_mapping(Some(UserMapping::default()))
            ));
            assert!(ownership_requested(
                &plain().with_group_mapping(Some(GroupMapping::default()))
            ));
        }
    }

    #[test]
    fn permissions_requested_tracks_perm_chmod_and_exec() {
        assert!(!permissions_beyond_readonly_requested(&plain()));

        assert!(permissions_beyond_readonly_requested(
            &plain().preserve_permissions(true)
        ));
        assert!(permissions_beyond_readonly_requested(
            &plain().preserve_executability(true)
        ));
        assert!(permissions_beyond_readonly_requested(
            &plain().with_chmod(Some(ChmodModifiers::default()))
        ));
    }

    #[test]
    fn warning_bodies_name_the_class_and_platform() {
        assert!(OWNERSHIP_WARNING.contains("ownership"));
        assert!(OWNERSHIP_WARNING.contains("Windows"));
        assert!(PERMISSIONS_WARNING.contains("read-only"));
        assert!(PERMISSIONS_WARNING.contains("Windows"));
    }

    #[test]
    fn warn_once_if_fires_exactly_once_when_requested() {
        let warned = AtomicBool::new(false);
        let count = Cell::new(0u32);
        warn_once_if(true, &warned, || count.set(count.get() + 1));
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn warn_once_if_stays_silent_when_not_requested() {
        let warned = AtomicBool::new(false);
        let count = Cell::new(0u32);
        // Model the many-files loop: repeated apply attempts, class not requested.
        for _ in 0..1000 {
            warn_once_if(false, &warned, || count.set(count.get() + 1));
        }
        assert_eq!(count.get(), 0);
        assert!(!warned.load(Ordering::Relaxed));
    }

    #[test]
    fn warn_once_if_fires_once_across_many_files() {
        let warned = AtomicBool::new(false);
        let count = Cell::new(0u32);
        for _ in 0..1000 {
            warn_once_if(true, &warned, || count.set(count.get() + 1));
        }
        assert_eq!(count.get(), 1);
    }
}
