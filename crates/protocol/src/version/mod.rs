//! Protocol version helpers mirroring upstream rsync 3.4.1 semantics.

mod advertisement;
mod constants;
mod iter;
mod parse;
mod protocol_version;
mod recognized;
mod select;

pub use advertisement::ProtocolVersionAdvertisement;
pub use constants::{SUPPORTED_PROTOCOL_BOUNDS, SUPPORTED_PROTOCOL_RANGE};
pub use iter::{SupportedProtocolNumbersIter, SupportedVersionsIter};
pub use parse::{ParseProtocolVersionError, ParseProtocolVersionErrorKind};
pub use protocol_version::{
    ProtocolVersion, SUPPORTED_PROTOCOL_BITMAP, SUPPORTED_PROTOCOL_COUNT, SUPPORTED_PROTOCOLS,
    SUPPORTED_PROTOCOLS_DISPLAY,
};
pub use select::select_highest_mutual;

#[cfg(test)]
mod tests;
