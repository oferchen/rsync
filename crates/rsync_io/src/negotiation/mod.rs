mod buffer;
mod errors;
mod parts;
mod sniffer;
mod stream;

pub use buffer::{BufferedCopyTooSmall, CopyToSliceError, NegotiationBufferedSlices};
pub use parts::{NegotiatedStreamParts, TryMapInnerError};
pub use sniffer::{sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer};
pub use stream::{NEGOTIATION_PROLOGUE_UNDETERMINED_MSG, NegotiatedStream};

pub(crate) use buffer::{NegotiationBuffer, NegotiationBufferAccess};
pub(crate) use errors::map_line_reserve_error_for_io;

#[cfg(test)]
mod tests;
