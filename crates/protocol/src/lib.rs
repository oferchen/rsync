//! Wire protocol implementation for rsync protocol versions 28-32.
//!
//! This crate implements the negotiation and multiplexing primitives for the
//! rsync wire protocol. It mirrors upstream rsync 3.4.1 behaviour so that
//! higher layers can negotiate protocol versions, interpret legacy daemon
//! banners, exchange multiplexed `MSG_*` frames, encode file lists, and
//! perform delta transfers without depending on the original C sources.
//!
//! ## Protocol Version Support
//!
//! Supported protocol versions: **28 through 32** (inclusive), matching
//! upstream rsync 2.6.x through 3.4.x. The negotiated version is represented
//! by [`ProtocolVersion`], and [`SUPPORTED_PROTOCOLS`] lists all supported
//! numbers in descending order. [`select_highest_mutual`] derives the
//! highest version both peers advertise.
//!
//! ## Key Modules
//!
//! - [`flist`] - File list encoding and decoding (file entries, attributes).
//! - [`codec`] - Protocol version-aware wire encoding (Strategy pattern).
//!   Includes [`codec::ProtocolCodec`] for general encoding and
//!   [`codec::NdxCodec`] for file-list index encoding.
//! - [`wire`] - Wire protocol serialization for signatures, deltas, and file
//!   entries.
//! - [`multiplex`] / `envelope` - `MSG_*` control/data framing used once a
//!   session is negotiated. [`MplexReader`] and [`MplexWriter`] handle the
//!   24-bit-payload multiplexed channel.
//! - [`varint`] - Variable-length integer codec matching upstream rsync's
//!   `varint`/`varlong` encoding.
//! - [`negotiation`] - Incremental sniffers that classify handshake style
//!   (binary vs. legacy ASCII) without losing buffered bytes.
//! - [`acl`] - ACL wire protocol encoding and decoding.
//! - [`xattr`] - Extended attribute wire protocol encoding and decoding.
//! - [`compatibility`] - Post-negotiation compatibility flags shared by peers.
//! - [`filters`] - Filter list wire protocol encoding and decoding.
//! - [`stats`] - Transfer and delete statistics wire format.
//! - [`state`] - Type-safe state machine for rsync protocol phases.
//!
//! ## Golden Byte Tests
//!
//! `tests/golden_handshakes.rs` contains golden byte tests that pin the exact
//! wire bytes for handshake sequences across all supported protocol versions.
//! Any change to wire format must update the golden fixtures and justify the
//! deviation from upstream rsync behaviour.
//!
//! ## Invariants
//!
//! - [`SUPPORTED_PROTOCOLS`] always lists protocol numbers in descending order
//!   (32 through 28).
//! - Legacy negotiation helpers never drop or duplicate bytes: sniffed
//!   prefixes can be replayed verbatim into the parsing routines.
//! - Multiplexed message headers clamp payload lengths to the 24-bit limit
//!   used by upstream rsync.
//!
//! ## Examples
//!
//! Detect negotiation style from a buffered prologue:
//!
//! ```rust
//! use protocol::{detect_negotiation_prologue, NegotiationPrologue};
//!
//! assert_eq!(
//!     detect_negotiation_prologue(b"@RSYNCD: 30.0\n"),
//!     NegotiationPrologue::LegacyAscii
//! );
//! assert_eq!(
//!     detect_negotiation_prologue(&[0x00, 0x20, 0x00, 0x00]),
//!     NegotiationPrologue::Binary
//! );
//! ```
//!
//! Derive the highest mutually supported protocol version:
//!
//! ```rust
//! use protocol::{select_highest_mutual, ProtocolVersion};
//!
//! let negotiated = select_highest_mutual([32, 31]).expect("mutual version exists");
//! assert_eq!(negotiated, ProtocolVersion::NEWEST);
//! ```
#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

/// ACL (Access Control List) wire protocol encoding and decoding.
pub mod acl;
/// Protocol version-aware encoding/decoding using the Strategy pattern.
///
/// This module includes both [`codec::ProtocolCodec`] for general wire encoding and
/// [`codec::NdxCodec`] for file-list index encoding. See [`codec`] for details.
pub mod codec;
mod compatibility;
/// Debug I/O tracing for protocol wire operations.
pub mod debug_io;
/// Debug tracing system for protocol analysis.
pub mod debug_trace;
mod envelope;
mod error;
/// Error recovery and partial transfer handling for rsync protocol operations.
pub mod error_recovery;
/// Wire protocol for `--files-from` file list forwarding between client and server.
pub mod files_from;
/// Filter list wire protocol encoding and decoding.
pub mod filters;
/// File list encoding and decoding.
pub mod flist;
/// Basis file comparison type constants for alternate basis selection.
pub mod fnamecmp;
/// Filename encoding conversion (iconv) for cross-platform transfers.
pub mod iconv;
/// UID/GID mapping lists for name-based ownership transfer.
pub mod idlist;
mod legacy;
mod multiplex;
mod negotiation;
/// Secluded-args (protect-args) stdin argument transmission protocol.
///
/// When `--protect-args` is active, arguments are sent over stdin as
/// null-separated strings instead of appearing on the remote command line.
pub mod secluded_args;
/// Type-safe state machine for rsync protocol phases.
pub mod state;
/// Transfer statistics wire format encoding and decoding.
pub mod stats;
mod varint;
mod version;
/// Wire protocol serialization for signatures, deltas, and file entries.
pub mod wire;
/// Extended attribute wire protocol encoding and decoding.
pub mod xattr;

pub use compatibility::{
    CompatibilityFlags, KnownCompatibilityFlag, KnownCompatibilityFlagsIter,
    ParseKnownCompatibilityFlagError,
};
pub use envelope::{
    EnvelopeError, HEADER_LEN as MESSAGE_HEADER_LEN, LogCode, LogCodeConversionError,
    MAX_PAYLOAD_LENGTH, MPLEX_BASE, MessageCode, MessageHeader, ParseLogCodeError,
    ParseMessageCodeError,
};
pub use error::NegotiationError;
pub use files_from::{forward_files_from, read_files_from_stream};
pub use fnamecmp::{FnameCmpType, InvalidFnameCmpType};
pub use iconv::{
    ConversionError, EncodingConverter, EncodingError, EncodingPair, FilenameConverter,
    converter_from_locale,
};
pub use legacy::{
    DigestListTokens, LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN,
    LegacyDaemonGreeting, LegacyDaemonGreetingOwned, LegacyDaemonMessage,
    format_legacy_daemon_greeting, format_legacy_daemon_message, parse_legacy_daemon_greeting,
    parse_legacy_daemon_greeting_bytes, parse_legacy_daemon_greeting_bytes_details,
    parse_legacy_daemon_greeting_bytes_owned, parse_legacy_daemon_greeting_details,
    parse_legacy_daemon_greeting_owned, parse_legacy_daemon_message,
    parse_legacy_daemon_message_bytes, parse_legacy_error_message,
    parse_legacy_error_message_bytes, parse_legacy_warning_message,
    parse_legacy_warning_message_bytes, write_legacy_daemon_greeting, write_legacy_daemon_message,
};
#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
pub use multiplex::MultiplexCodec;
pub use multiplex::{
    BorrowedMessageFrame, BorrowedMessageFrames, MessageFrame, MplexReader, MplexWriter, recv_msg,
    recv_msg_into, send_frame, send_keepalive, send_msg, send_msgs_vectored,
};
pub use negotiation::{
    BufferedPrefixTooSmall, ChecksumAlgorithm, CompressionAlgorithm, NegotiationPrologue,
    NegotiationPrologueDetector, NegotiationPrologueSniffer, NegotiationResult,
    ParseNegotiationPrologueError, ParseNegotiationPrologueErrorKind, detect_negotiation_prologue,
    negotiate_capabilities, negotiate_capabilities_with_override,
    read_and_parse_legacy_daemon_greeting, read_and_parse_legacy_daemon_greeting_details,
    read_legacy_daemon_line,
};
pub use stats::{DeleteStats, TransferStats};
pub use varint::{
    decode_varint, encode_varint_to_vec, read_int, read_longint, read_varint, read_varint30_int,
    read_varlong, read_varlong30, write_int, write_longint, write_varint, write_varint30_int,
    write_varlong, write_varlong30,
};
pub use version::{
    ParseProtocolVersionError, ParseProtocolVersionErrorKind, ProtocolVersion,
    ProtocolVersionAdvertisement, SUPPORTED_PROTOCOL_BITMAP, SUPPORTED_PROTOCOL_BOUNDS,
    SUPPORTED_PROTOCOL_COUNT, SUPPORTED_PROTOCOL_RANGE, SUPPORTED_PROTOCOLS,
    SUPPORTED_PROTOCOLS_DISPLAY, SupportedProtocolNumbersIter, SupportedVersionsIter,
    select_highest_mutual,
};
