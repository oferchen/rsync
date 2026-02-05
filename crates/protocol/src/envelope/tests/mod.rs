pub(super) use super::{
    EnvelopeError, HEADER_LEN, LogCode, LogCodeConversionError, MAX_PAYLOAD_LENGTH, MPLEX_BASE,
    MessageCode, MessageHeader, PAYLOAD_MASK, ParseLogCodeError, ParseMessageCodeError,
};

mod codes;
mod conversions;
mod header;
mod message_code_comprehensive;
mod properties;
