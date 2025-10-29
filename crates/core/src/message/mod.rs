pub mod strings;

mod errors;
mod macros;
mod message_impl;
mod numbers;
mod role;
mod scratch;
mod segments;
mod severity;
mod source;
#[cfg(test)]
mod tests;

pub use message_impl::Message;
pub use role::{ParseRoleError, Role};
pub use scratch::MessageScratch;
pub use segments::{CopyToSliceError, MessageSegments};
pub use severity::{ParseSeverityError, Severity};
pub use source::SourceLocation;

/// Version tag appended to message trailers.
pub const VERSION_SUFFIX: &str = crate::version::RUST_VERSION;
pub(super) const MAX_MESSAGE_SEGMENTS: usize = 18;
pub(super) const OVERREPORTED_BYTES_ERROR: &str =
    "writer reported more bytes than available in message";
