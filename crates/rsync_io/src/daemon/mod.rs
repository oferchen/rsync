//! Legacy ASCII daemon negotiation (`@RSYNCD:` protocol).
//!
//! This module implements the `@RSYNCD:` text-based handshake used by daemon
//! connections (`rsync://`). It parses the server greeting, negotiates the
//! protocol version, and echoes the client banner before returning the
//! replaying stream.

mod negotiate;
mod types;

pub use negotiate::{
    negotiate_legacy_daemon_session, negotiate_legacy_daemon_session_from_stream,
    negotiate_legacy_daemon_session_with_sniffer,
};
pub use types::{LegacyDaemonHandshake, LegacyDaemonHandshakeParts};

#[cfg(test)]
mod tests;
