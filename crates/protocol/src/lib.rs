#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! Protocol version negotiation helpers for the Rust `rsync` reimplementation.
//!
//! The crate is split into small modules that mirror upstream rsync's
//! negotiation building blocks. Re-exported APIs allow higher layers to remain
//! agnostic to the internal layout while benefitting from the reduced file
//! sizes required by the workspace style guide.

mod envelope;
mod error;
mod legacy;
mod multiplex;
mod negotiation;
mod version;

pub use envelope::{
    EnvelopeError, HEADER_LEN as MESSAGE_HEADER_LEN, MAX_PAYLOAD_LENGTH, MessageCode, MessageHeader,
};
pub use error::NegotiationError;
pub use legacy::{
    LegacyDaemonMessage, format_legacy_daemon_greeting, parse_legacy_daemon_greeting,
    parse_legacy_daemon_greeting_bytes, parse_legacy_daemon_message,
    parse_legacy_daemon_message_bytes, parse_legacy_error_message,
    parse_legacy_error_message_bytes, parse_legacy_warning_message,
    parse_legacy_warning_message_bytes,
};
pub use multiplex::{MessageFrame, recv_msg, send_msg};
pub use negotiation::{
    NegotiationPrologue, NegotiationPrologueDetector, detect_negotiation_prologue,
};
pub use version::{ProtocolVersion, SUPPORTED_PROTOCOLS, select_highest_mutual};
