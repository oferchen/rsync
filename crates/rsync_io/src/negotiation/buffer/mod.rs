mod access;
mod errors;
mod slices;
mod storage;

pub use errors::{BufferedCopyTooSmall, CopyToSliceError};
pub use slices::NegotiationBufferedSlices;

pub(crate) use access::NegotiationBufferAccess;
pub(crate) use storage::NegotiationBuffer;
