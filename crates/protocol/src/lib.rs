#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! # Overview
//!
//! `rsync_protocol` implements the negotiation and multiplexing primitives
//! required by the Rust `rsync` reimplementation. The crate mirrors upstream
//! rsync 3.4.1 behaviour so higher layers can negotiate protocol versions,
//! interpret legacy daemon banners, and exchange multiplexed frames without
//! depending on the original C sources.
//!
//! # Design
//!
//! Functionality is decomposed into modules that track the upstream layout:
//!
//! - [`version`] exposes [`ProtocolVersion`] and helpers for selecting the
//!   highest mutually supported protocol between peers.
//! - [`legacy`] provides parsers for ASCII daemon handshakes such as
//!   `@RSYNCD: 31.0` and the follow-up control messages emitted by rsync
//!   daemons prior to protocol 30.
//! - [`negotiation`] includes incremental sniffers that classify the handshake
//!   style (binary vs legacy ASCII) without losing buffered bytes.
//! - [`multiplex`] and [`envelope`] re-create the control/data framing used once
//!   a session has been negotiated.
//! - [`compatibility`] models the post-negotiation compatibility flags shared
//!   by peers and exposes typed helpers for working with individual bits.
//! - [`varint`] reproduces rsync's variable-length integer codec so other
//!   modules can serialise the compatibility flags and future protocol values.
//!
//! Each module is small enough to satisfy the workspace style guide while the
//! crate root re-exports the stable APIs consumed by the higher-level
//! transport, core, and daemon layers.
//!
//! # Invariants
//!
//! - [`SUPPORTED_PROTOCOLS`] always lists protocol numbers in descending order
//!   (`32` through `28`).
//! - Legacy negotiation helpers never drop or duplicate bytes: sniffed prefixes
//!   can be replayed verbatim into the parsing routines.
//! - Multiplexed message headers clamp payload lengths to the 24-bit limit used
//!   by upstream rsync.
//!
//! # Errors
//!
//! Parsing helpers surface rich error types that carry enough context to
//! reproduce upstream diagnostics. For example,
//! [`NegotiationError`](error::NegotiationError) distinguishes between malformed
//! greetings, unsupported protocol ranges, and truncated payloads. All error
//! types implement [`std::error::Error`] and convert into [`std::io::Error`]
//! where appropriate so they integrate naturally with transport code.
//!
//! # Examples
//!
//! Determine whether a buffered prologue belongs to the legacy ASCII greeting
//! or the binary negotiation. The helper behaves exactly like upstream rsync's
//! `io.c:check_protok` logic by classifying the session based on the first byte.
//!
//! ```
//! use rsync_protocol::{detect_negotiation_prologue, NegotiationPrologue};
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
//! Once the negotiation style is known, the highest mutually supported protocol
//! can be derived from the peer advertisement.
//!
//! ```
//! use rsync_protocol::{select_highest_mutual, ProtocolVersion};
//!
//! let negotiated = select_highest_mutual([32, 31]).expect("mutual version exists");
//! assert_eq!(negotiated, ProtocolVersion::NEWEST);
//! ```
//!
//! When a peer selects the legacy ASCII negotiation, the bytes that triggered
//! the decision must be replayed into the greeting parser so the full
//! `@RSYNCD:` line can be reconstructed. [`NegotiationPrologueSniffer`] owns the
//! buffered prefix, allowing callers to reuse it without copying more data than
//! upstream rsync would have consumed.
//!
//! ```
//! use rsync_protocol::{
//!     NegotiationPrologue, NegotiationPrologueSniffer, parse_legacy_daemon_greeting,
//! };
//! use std::io::{Cursor, Read};
//!
//! let mut reader = Cursor::new(&b"@RSYNCD: 31.0\n"[..]);
//! let mut sniffer = NegotiationPrologueSniffer::new();
//!
//! let decision = sniffer
//!     .read_from(&mut reader)
//!     .expect("sniffing never fails for in-memory data");
//! assert_eq!(decision, NegotiationPrologue::LegacyAscii);
//!
//! let mut prefix = Vec::new();
//! sniffer
//!     .take_buffered_into(&mut prefix)
//!     .expect("the vector has enough capacity for @RSYNCD:");
//! assert_eq!(prefix, b"@RSYNCD:");
//!
//! let mut full_line = prefix;
//! reader.read_to_end(&mut full_line).expect("cursor read cannot fail");
//! assert_eq!(full_line, b"@RSYNCD: 31.0\n");
//! assert_eq!(
//!     parse_legacy_daemon_greeting(std::str::from_utf8(&full_line).unwrap())
//!         .expect("banner is well-formed"),
//!     rsync_protocol::ProtocolVersion::from_supported(31).unwrap()
//! );
//! ```
//!
//! # See also
//!
//! - [`rsync_transport`] for transport wrappers that reuse the sniffers and
//!   parsers exposed here.
//! - [`rsync_core`] for message formatting utilities that rely on negotiated
//!   protocol numbers.

mod compatibility;
mod envelope;
mod error;
mod legacy;
mod multiplex;
mod negotiation;
mod varint;
mod version;

pub use compatibility::{CompatibilityFlags, KnownCompatibilityFlag};
pub use envelope::{
    EnvelopeError, HEADER_LEN as MESSAGE_HEADER_LEN, LogCode, LogCodeConversionError,
    MAX_PAYLOAD_LENGTH, MPLEX_BASE, MessageCode, MessageHeader, ParseLogCodeError,
    ParseMessageCodeError,
};
pub use error::NegotiationError;
pub use legacy::{
    LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN,
    LegacyDaemonGreeting, LegacyDaemonGreetingOwned, LegacyDaemonMessage,
    format_legacy_daemon_greeting, parse_legacy_daemon_greeting,
    parse_legacy_daemon_greeting_bytes, parse_legacy_daemon_greeting_bytes_details,
    parse_legacy_daemon_greeting_bytes_owned, parse_legacy_daemon_greeting_details,
    parse_legacy_daemon_greeting_owned, parse_legacy_daemon_message,
    parse_legacy_daemon_message_bytes, parse_legacy_error_message,
    parse_legacy_error_message_bytes, parse_legacy_warning_message,
    parse_legacy_warning_message_bytes, write_legacy_daemon_greeting,
};
pub use multiplex::{
    BorrowedMessageFrame, MessageFrame, recv_msg, recv_msg_into, send_frame, send_msg,
};
pub use negotiation::{
    BufferedPrefixTooSmall, NegotiationPrologue, NegotiationPrologueDetector,
    NegotiationPrologueSniffer, ParseNegotiationPrologueError, ParseNegotiationPrologueErrorKind,
    detect_negotiation_prologue, read_and_parse_legacy_daemon_greeting,
    read_and_parse_legacy_daemon_greeting_details, read_legacy_daemon_line,
};
pub use varint::{decode_varint, encode_varint_to_vec, read_varint, write_varint};
pub use version::{
    ParseProtocolVersionError, ParseProtocolVersionErrorKind, ProtocolVersion,
    ProtocolVersionAdvertisement, SUPPORTED_PROTOCOL_BITMAP, SUPPORTED_PROTOCOL_BOUNDS,
    SUPPORTED_PROTOCOL_COUNT, SUPPORTED_PROTOCOL_RANGE, SUPPORTED_PROTOCOLS,
    SupportedProtocolNumbersIter, SupportedVersionsIter, select_highest_mutual,
};
