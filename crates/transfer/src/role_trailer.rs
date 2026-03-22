//! Role trailer and source location formatting for error and warning messages.
//!
//! Upstream rsync appends role trailers like `[sender=3.4.1]` and source
//! locations like `at io.c(234)` to diagnostic messages. This module provides
//! the same formatting for oc-rsync's transfer roles.
//!
//! # Upstream Reference
//!
//! - `log.c:rwrite()` - appends `at <file>(<line>) [role=VERSION]` to error/warning lines

/// Package version used in role trailers, matching `RUST_VERSION` from branding.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Extracts the filename component from a path string.
///
/// Handles both Unix (`/`) and Windows (`\`) separators.
pub(crate) fn file_basename(path: &str) -> &str {
    path.rsplit_once('/')
        .or_else(|| path.rsplit_once('\\'))
        .map_or(path, |(_, name)| name)
}

/// Produces an upstream-compatible source location string.
///
/// Upstream rsync includes source file and line number in error messages using
/// the format `at <basename>(<line>)` - for example `at io.c(234)`. This macro
/// expands to a `String` matching that format, using the basename of the
/// current source file and the call-site line number.
///
/// # Upstream Reference
///
/// - `log.c:rwrite()` - formats the `at <file>(<line>)` suffix
macro_rules! error_location {
    () => {
        format!(
            "at {}({})",
            $crate::role_trailer::file_basename(file!()),
            line!(),
        )
    };
}

pub(crate) use error_location;

/// Returns the role trailer suffix for the sender role.
pub(crate) fn sender() -> String {
    format!(" [sender={VERSION}]")
}

/// Returns the role trailer suffix for the receiver role.
pub(crate) fn receiver() -> String {
    format!(" [receiver={VERSION}]")
}

/// Returns the role trailer suffix for the generator role.
///
/// Upstream rsync uses `[generator=VERSION]` for messages emitted by the
/// generator process - see `log.c:who_am_i()`.
pub(crate) fn generator() -> String {
    format!(" [generator={VERSION}]")
}

/// Returns the role trailer suffix for the daemon role.
#[allow(dead_code)]
pub(crate) fn daemon() -> String {
    format!(" [daemon={VERSION}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_basename_extracts_unix_path() {
        assert_eq!(file_basename("crates/transfer/src/role_trailer.rs"), "role_trailer.rs");
    }

    #[test]
    fn file_basename_extracts_windows_path() {
        assert_eq!(file_basename("crates\\transfer\\src\\main.rs"), "main.rs");
    }

    #[test]
    fn file_basename_returns_bare_filename() {
        assert_eq!(file_basename("lib.rs"), "lib.rs");
    }

    #[test]
    fn error_location_format_matches_upstream() {
        let location = error_location!();
        assert!(location.starts_with("at "), "should start with 'at ': {location}");
        assert!(location.contains(".rs("), "should contain '.rs(': {location}");
        assert!(location.ends_with(')'), "should end with ')': {location}");
    }

    #[test]
    fn sender_trailer_contains_role_and_version() {
        let trailer = sender();
        assert!(trailer.contains("sender="));
        assert!(trailer.contains(VERSION));
    }

    #[test]
    fn receiver_trailer_contains_role_and_version() {
        let trailer = receiver();
        assert!(trailer.contains("receiver="));
        assert!(trailer.contains(VERSION));
    }

    #[test]
    fn generator_trailer_contains_role_and_version() {
        let trailer = generator();
        assert!(trailer.contains("generator="));
        assert!(trailer.contains(VERSION));
    }

    #[test]
    fn daemon_trailer_contains_role_and_version() {
        let trailer = daemon();
        assert!(trailer.contains("daemon="));
        assert!(trailer.contains(VERSION));
    }

    #[test]
    fn sender_trailer_format_matches_upstream() {
        let trailer = sender();
        assert!(trailer.starts_with(" [sender="));
        assert!(trailer.ends_with(']'));
    }

    #[test]
    fn receiver_trailer_format_matches_upstream() {
        let trailer = receiver();
        assert!(trailer.starts_with(" [receiver="));
        assert!(trailer.ends_with(']'));
    }

    #[test]
    fn generator_trailer_format_matches_upstream() {
        let trailer = generator();
        assert!(trailer.starts_with(" [generator="));
        assert!(trailer.ends_with(']'));
    }

    #[test]
    fn daemon_trailer_format_matches_upstream() {
        let trailer = daemon();
        assert!(trailer.starts_with(" [daemon="));
        assert!(trailer.ends_with(']'));
    }
}
