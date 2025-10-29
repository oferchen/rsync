#![allow(clippy::module_name_repetitions)]

pub(super) use super::constants::UPSTREAM_PROTOCOL_RANGE;
pub(super) use super::{
    ParseProtocolVersionError, ParseProtocolVersionErrorKind, ProtocolVersion,
    ProtocolVersionAdvertisement, SUPPORTED_PROTOCOL_BITMAP, SUPPORTED_PROTOCOL_BOUNDS,
    SUPPORTED_PROTOCOL_COUNT, SUPPORTED_PROTOCOL_RANGE, SUPPORTED_PROTOCOLS, select_highest_mutual,
};

mod advertisement;
mod common;
mod property;
mod protocol;
mod selection;
