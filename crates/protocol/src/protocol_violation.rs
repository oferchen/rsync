//! Marker error for genuine rsync protocol violations (RERR_PROTOCOL).
//!
//! Rust's [`std::io::ErrorKind`] has no protocol-violation variant, so oc has
//! historically encoded wire-protocol violations as
//! [`io::ErrorKind::InvalidData`]. That kind is shared with truncated-stream
//! and corrupt-frame conditions, which upstream rsync exits with
//! `RERR_STREAMIO` (12). A subset of `InvalidData` sites correspond instead to
//! upstream call sites that invoke `exit_cleanup(RERR_PROTOCOL)` (exit code 2)
//! - for example an out-of-range wire index, a malformed `sum_head`, or a
//! transfer request arriving in phase 2.
//!
//! [`ProtocolViolation`] tags exactly those sites. It is attached as the inner
//! error of an `InvalidData` [`io::Error`] via [`protocol_violation`], so the
//! error's [`kind`](io::Error::kind) and [`Display`](std::fmt::Display) text are
//! unchanged (fully backward compatible), while the exit-code mapper can
//! downcast the inner error and return `RERR_PROTOCOL` (2) rather than
//! `RERR_STREAMIO` (12).
//!
//! # Upstream Reference
//!
//! `errcode.h` - `RERR_PROTOCOL = 2` (protocol incompatibility) versus
//! `RERR_STREAMIO = 12` (error in rsync protocol data stream). Upstream
//! distinguishes them at the call site; oc distinguishes them via this marker.

use std::error::Error;
use std::fmt;
use std::io;

/// Inner marker error identifying an [`io::Error`] as a genuine protocol
/// violation that upstream rsync would exit with `RERR_PROTOCOL` (2).
///
/// Constructed via [`protocol_violation`] and detected by the core exit-code
/// mapper. Its [`Display`](fmt::Display) renders exactly the wrapped message,
/// so wrapping an existing diagnostic changes neither the error text nor its
/// [`io::ErrorKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolViolation(pub String);

impl fmt::Display for ProtocolViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for ProtocolViolation {}

/// Builds an [`io::Error`] of kind [`InvalidData`](io::ErrorKind::InvalidData)
/// tagged as a [`ProtocolViolation`].
///
/// Use this at wire-protocol sites that mirror an upstream
/// `exit_cleanup(RERR_PROTOCOL)` call. The resulting error displays as `msg`
/// and reports [`io::ErrorKind::InvalidData`], exactly like a plain
/// `io::Error::new(InvalidData, msg)`, but the core exit-code mapper maps it to
/// `RERR_PROTOCOL` (2) instead of `RERR_STREAMIO` (12).
///
/// # Upstream Reference
///
/// `errcode.h` - `RERR_PROTOCOL = 2`.
pub fn protocol_violation(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, ProtocolViolation(msg.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_kind_and_display() {
        // WHY: wrapping must be observationally identical to a plain
        // InvalidData error so existing callers that read `.kind()` or format
        // the message keep their exact behaviour; only the exit-code mapping
        // may change.
        let err = protocol_violation("got transfer request in phase 2 [sender]");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "got transfer request in phase 2 [sender]");
    }

    #[test]
    fn inner_downcasts_to_marker() {
        // WHY: the core exit-code mapper identifies the RERR_PROTOCOL class by
        // downcasting the inner error to ProtocolViolation. If this stops
        // working the violation silently reverts to RERR_STREAMIO (12).
        let err = protocol_violation("malformed sum_head");
        let inner = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<ProtocolViolation>());
        assert_eq!(inner, Some(&ProtocolViolation("malformed sum_head".into())));
    }
}
