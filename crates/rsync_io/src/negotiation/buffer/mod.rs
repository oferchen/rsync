//! Replay buffer used during negotiation prologue detection.
//!
//! Bytes read while sniffing the connection are stored here and replayed
//! to higher layers through [`NegotiationBufferAccess`].

mod access;
mod errors;
mod slices;
mod storage;

pub use errors::{BufferedCopyTooSmall, CopyToSliceError};
pub use slices::NegotiationBufferedSlices;

pub(crate) use access::NegotiationBufferAccess;
pub(crate) use storage::NegotiationBuffer;
