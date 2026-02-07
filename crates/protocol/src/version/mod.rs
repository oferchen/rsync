//! Protocol version representation, negotiation, and feature queries.
//!
//! This module provides [`ProtocolVersion`] -- the strongly typed wrapper for
//! rsync protocol version numbers -- together with helpers for selecting the
//! highest mutually supported version ([`select_highest_mutual`]) and iterating
//! over the supported set.
//!
//! The supported versions (28 through 32) match upstream rsync 3.4.1. Version
//! 30 is the boundary between the legacy ASCII negotiation and the modern
//! binary handshake.

mod advertisement;
mod constants;
mod iter;
mod parse;
mod protocol_version;
mod recognized;
mod select;

pub use advertisement::ProtocolVersionAdvertisement;
#[allow(unused_imports)]
pub use constants::{
    MAXIMUM_PROTOCOL_ADVERTISEMENT, SUPPORTED_PROTOCOL_BOUNDS, SUPPORTED_PROTOCOL_RANGE,
};
pub use iter::{SupportedProtocolNumbersIter, SupportedVersionsIter};
pub use parse::{ParseProtocolVersionError, ParseProtocolVersionErrorKind};
pub use protocol_version::{
    ProtocolVersion, SUPPORTED_PROTOCOL_BITMAP, SUPPORTED_PROTOCOL_COUNT, SUPPORTED_PROTOCOLS,
    SUPPORTED_PROTOCOLS_DISPLAY,
};
pub use select::select_highest_mutual;

#[cfg(test)]
mod tests;
