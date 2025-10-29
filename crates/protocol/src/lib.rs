#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

mod compatibility;
mod envelope;
mod error;
mod legacy;
mod multiplex;
mod negotiation;
mod varint;
mod version;

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
pub use multiplex::{
    BorrowedMessageFrame, BorrowedMessageFrames, MessageFrame, recv_msg, recv_msg_into, send_frame,
    send_msg,
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
    SUPPORTED_PROTOCOLS_DISPLAY, SupportedProtocolNumbersIter, SupportedVersionsIter,
    select_highest_mutual,
};
