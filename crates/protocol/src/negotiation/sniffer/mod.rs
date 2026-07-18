//! Buffered sniffer that reads from a transport until the negotiation style is
//! known, owning the consumed prefix so callers can replay it into the legacy
//! greeting parser.
//!
//! Wraps `NegotiationPrologueDetector` with an owned buffer and drain helpers.
//! Split into focused submodules:
//!
//! - `core` - the `NegotiationPrologueSniffer` type and its accessors
//! - `observe` - feeding transport bytes and reading until decided
//! - `legacy` - reading and parsing the `@RSYNCD:` daemon greeting
//! - `drain` - replaying the buffered prefix and remainder into caller storage
//! - `util` - shared vectored-copy and capacity helpers
//! - `async_read` - tokio `AsyncRead` support (feature `async`)

mod core;
mod drain;
mod legacy;
mod observe;
mod util;

#[cfg(feature = "async")]
mod async_read;

pub use core::NegotiationPrologueSniffer;
pub use legacy::{
    read_and_parse_legacy_daemon_greeting, read_and_parse_legacy_daemon_greeting_details,
    read_legacy_daemon_line,
};

pub(crate) use legacy::map_reserve_error_for_io;
