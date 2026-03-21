//! Role trailer formatting for error and warning messages.
//!
//! Upstream rsync appends role trailers like `[sender=3.4.1]` to diagnostic
//! messages so users can identify which process produced the output. This
//! module provides the same formatting for oc-rsync's transfer roles.
//!
//! # Upstream Reference
//!
//! - `log.c:rwrite()` - appends `[role=VERSION]` to error/warning lines

/// Package version used in role trailers, matching `RUST_VERSION` from branding.
const VERSION: &str = env!("CARGO_PKG_VERSION");

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
