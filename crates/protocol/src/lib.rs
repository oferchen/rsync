#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! Protocol version negotiation helpers for the Rust `rsync` reimplementation.
//!
//! The crate is split into small modules that mirror upstream rsync's
//! negotiation building blocks. Re-exported APIs allow higher layers to remain
//! agnostic to the internal layout while benefitting from the reduced file
//! sizes required by the workspace style guide. The utilities exposed here cover
//! both the initial protocol handshake and the multiplexed control stream used
//! after a session has been negotiated.
//!
//! # Examples
//!
//! Determine whether a buffered prologue belongs to the legacy ASCII greeting or
//! the binary negotiation. The helper behaves exactly like upstream rsync's
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

mod envelope;
mod error;
mod legacy;
mod multiplex;
mod negotiation;
mod version;

pub use envelope::{
    EnvelopeError, HEADER_LEN as MESSAGE_HEADER_LEN, LogCode, LogCodeConversionError,
    MAX_PAYLOAD_LENGTH, MessageCode, MessageHeader, ParseLogCodeError, ParseMessageCodeError,
};
pub use error::NegotiationError;
pub use legacy::{
    LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN,
    LegacyDaemonMessage, format_legacy_daemon_greeting, parse_legacy_daemon_greeting,
    parse_legacy_daemon_greeting_bytes, parse_legacy_daemon_message,
    parse_legacy_daemon_message_bytes, parse_legacy_error_message,
    parse_legacy_error_message_bytes, parse_legacy_warning_message,
    parse_legacy_warning_message_bytes,
};
pub use multiplex::{MessageFrame, recv_msg, recv_msg_into, send_msg};
pub use negotiation::{
    BufferedPrefixTooSmall, NegotiationPrologue, NegotiationPrologueDetector,
    NegotiationPrologueSniffer, detect_negotiation_prologue, read_legacy_daemon_line,
};
pub use version::{
    ProtocolVersion, ProtocolVersionAdvertisement, SUPPORTED_PROTOCOL_BOUNDS,
    SUPPORTED_PROTOCOL_COUNT, SUPPORTED_PROTOCOL_RANGE, SUPPORTED_PROTOCOLS, select_highest_mutual,
};
