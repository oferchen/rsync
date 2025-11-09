#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! # Overview
//!
//! `transport` houses the transport adapters used by the Rust `rsync`
//! implementation. The crate currently focuses on handshake detection and
//! exposes wrappers that preserve bytes consumed while deciding between legacy
//! ASCII and binary negotiations.
//!
//! # Design
//!
//! The public modules mirror upstream rsync's layering:
//!
//! - [`sniff_negotiation_stream`] and [`NegotiatedStream`] classify the prologue
//!   without losing buffered data.
//! - [`negotiate_binary_session`] and [`negotiate_legacy_daemon_session`] wrap
//!   the protocol helpers to perform client and daemon handshakes.
//! - [`parse_remote_shell`] spawns a subprocess (typically the system `ssh`
//!   binary) and exposes its standard I/O handles through a [`std::io::Read`]
//!   /[`std::io::Write`] pair so the negotiation layer can treat remote-shell
//!   sessions like any other stream.
//! - [`SessionHandshake`] builds on top of both flows to expose a high-level
//!   session negotiation entry point.
//!
//! Each module is structured as a facade over the `protocol` crate, making
//! it possible to slot different transports (SSH stdio vs TCP daemon) behind the
//! same interface.
//!
//! # Invariants
//!
//! - Sniffed bytes are replayed exactly once; they are never duplicated or
//!   dropped when the negotiated stream is read.
//! - Legacy ASCII negotiations require the canonical `@RSYNCD:` prefix before
//!   exposing banner parsing APIs.
//! - `NegotiatedStream::try_map_inner` always preserves the original stream on
//!   failure, preventing state loss.
//!
//! # Errors
//!
//! Transport helpers surface [`std::io::Error`] values directly. When mapping
//! streams fails, [`TryMapInnerError`] retains the original value so callers can
//! recover without repeating the negotiation phase.
//!
//! # Examples
//!
//! Detect the negotiation style from an in-memory stream and read the replayed
//! prefix.
//!
//! ```
//! use transport::sniff_negotiation_stream;
//! use std::io::Cursor;
//!
//! let cursor = Cursor::new(&b"@RSYNCD: 31.0\n"[..]);
//! let negotiated = sniff_negotiation_stream(cursor).expect("sniffing succeeds");
//!
//! assert!(negotiated.buffered().starts_with(b"@RSYNCD:"));
//! assert!(negotiated.decision().is_legacy());
//! ```
//!
//! # See also
//!
//! - [`protocol`] for the negotiation parsers that back these adapters.
//! - `core` for the message helpers used when transport-level errors are
//!   reported to the user.

mod binary;
mod daemon;
mod handshake_util;
mod negotiation;
mod session;
pub mod ssh;

pub use binary::{
    BinaryHandshake, BinaryHandshakeParts, negotiate_binary_session,
    negotiate_binary_session_from_stream, negotiate_binary_session_with_sniffer,
};
pub use daemon::{
    LegacyDaemonHandshake, LegacyDaemonHandshakeParts, negotiate_legacy_daemon_session,
    negotiate_legacy_daemon_session_from_stream, negotiate_legacy_daemon_session_with_sniffer,
};
pub use handshake_util::{RemoteProtocolAdvertisement, local_cap_reduced_protocol};
pub use negotiation::{
    BufferedCopyTooSmall, CopyToSliceError, NegotiatedStream, NegotiatedStreamParts,
    TryMapInnerError, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
pub use session::{
    SessionHandshake, SessionHandshakeParts, negotiate_session, negotiate_session_from_stream,
    negotiate_session_parts, negotiate_session_parts_from_stream,
    negotiate_session_parts_with_sniffer, negotiate_session_with_sniffer,
};
pub use ssh::{RemoteShellParseError, SshCommand, SshConnection, parse_remote_shell};
