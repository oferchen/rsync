#![deny(unsafe_code)]

//! Client configuration data structures and helpers.
//!
//! This module isolates the data types used to describe transfer requests so
//! that they remain accessible to both the CLI front-end and daemon entry
//! points without keeping the primary orchestration module monolithic. All
//! definitions are re-exported from [`crate::client`] to preserve the existing
//! public API.

mod bandwidth;
mod builder;
mod client;
mod compress_env;
mod enums;
mod filters;
mod network;
mod reference;
mod skip_compress;

pub use bandwidth::BandwidthLimit;
pub use builder::ClientConfigBuilder;
pub use client::ClientConfig;
pub use compress_env::force_no_compress_from_env;
pub use enums::{
    AddressMode, CompressionSetting, DeleteMode, HumanReadableMode, HumanReadableModeParseError,
    StrongChecksumAlgorithm, StrongChecksumChoice, TransferTimeout,
};
pub use filters::{FilterRuleKind, FilterRuleSpec};
pub use network::BindAddress;
pub use reference::{ReferenceDirectory, ReferenceDirectoryKind};
pub use skip_compress::{parse_skip_compress_list, skip_compress_from_env};
