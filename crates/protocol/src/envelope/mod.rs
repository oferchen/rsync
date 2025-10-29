mod constants;
mod conversion;
mod error;
mod header;
mod log_code;
mod message_code;

pub use constants::{HEADER_LEN, MAX_PAYLOAD_LENGTH, MPLEX_BASE};
pub use conversion::LogCodeConversionError;
pub use error::EnvelopeError;
pub use header::MessageHeader;
pub use log_code::{LogCode, ParseLogCodeError};
pub use message_code::{MessageCode, ParseMessageCodeError};

pub(crate) use constants::PAYLOAD_MASK;

#[cfg(test)]
mod tests;
