//! Marker error for option-usage refusals (RERR_SYNTAX).
//!
//! Rust's [`std::io::ErrorKind`] has no syntax/usage variant, so an option
//! refusal raised from a transfer role would otherwise fall through the
//! exit-code mapper's catch-all and report `RERR_FILEIO` (11). A subset of
//! those sites correspond to upstream call sites that invoke
//! `exit_cleanup(RERR_SYNTAX)` (exit code 1) - for example a daemon receiver
//! refusing the options the client asked for.
//!
//! [`SyntaxViolation`] tags exactly those sites. It is attached as the inner
//! error of an [`InvalidInput`](std::io::ErrorKind::InvalidInput)
//! [`std::io::Error`] via [`fn@syntax_violation`], so the error's
//! [`kind`](std::io::Error::kind) and [`Display`](std::fmt::Display) text stay
//! ordinary, while the exit-code mapper can downcast the inner error and return
//! `RERR_SYNTAX` (1).
//!
//! This is the sibling of [`ProtocolViolation`](crate::ProtocolViolation),
//! which selects `RERR_PROTOCOL` the same way.
//!
//! # Upstream Reference
//!
//! `errcode.h` - `RERR_SYNTAX = 1` (syntax or usage error). Upstream selects it
//! at the call site via `exit_cleanup(RERR_SYNTAX)`; oc selects it via this
//! marker.

use std::error::Error;
use std::fmt;
use std::io;

/// Inner marker error identifying an [`io::Error`] as an option-usage refusal
/// that upstream rsync would exit with `RERR_SYNTAX` (1).
///
/// Constructed via [`syntax_violation`] and detected by the core exit-code
/// mapper. Its [`Display`](fmt::Display) renders exactly the wrapped message,
/// so wrapping a diagnostic changes neither the error text nor its
/// [`io::ErrorKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxViolation(pub String);

impl fmt::Display for SyntaxViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for SyntaxViolation {}

/// Builds an [`io::Error`] of kind [`InvalidInput`](io::ErrorKind::InvalidInput)
/// tagged as a [`SyntaxViolation`].
///
/// Use this at sites that mirror an upstream `exit_cleanup(RERR_SYNTAX)` call.
/// Without the tag the error would map to `RERR_FILEIO` (11) via the mapper's
/// catch-all arm.
///
/// # Upstream Reference
///
/// `errcode.h` - `RERR_SYNTAX = 1`.
pub fn syntax_violation(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, SyntaxViolation(msg.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_kind_and_display() {
        // WHY: the wrapper must stay observationally identical to a plain
        // InvalidInput error so callers that read `.kind()` or format the
        // message are unaffected; only the exit-code mapping may change.
        let err = syntax_violation("Your options have been rejected by the server.");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            "Your options have been rejected by the server."
        );
    }

    #[test]
    fn inner_downcasts_to_marker() {
        // WHY: the core exit-code mapper identifies the RERR_SYNTAX class by
        // downcasting the inner error. If this stops working the refusal
        // silently reverts to RERR_FILEIO (11) - upstream exits 1.
        let err = syntax_violation("rejected");
        let inner = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<SyntaxViolation>());
        assert_eq!(inner, Some(&SyntaxViolation("rejected".into())));
    }
}
