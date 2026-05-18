//! Hard link tracking for local copy operations.
//!
//! Provides two tracking strategies:
//!
//! - [`HardLinkTracker`] - source-side tracking by (device, inode) for local
//!   copies where the source filesystem exposes inode metadata.
//! - [`HardlinkApplyTracker`] - receiver-side tracking by `hardlink_idx` (gnum)
//!   for protocol 30+ transfers where hardlink groups are identified by wire
//!   index rather than filesystem metadata.

mod cohort;
mod tracker;

#[cfg(unix)]
mod unix;

#[cfg(not(unix))]
mod windows;

pub use cohort::{HardlinkApplyResult, HardlinkApplyTracker};
pub(crate) use tracker::HardLinkTracker;

#[cfg(test)]
mod tests;
