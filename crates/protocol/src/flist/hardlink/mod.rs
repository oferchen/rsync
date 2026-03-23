//! Hardlink tracking and deduplication for rsync protocol.
//!
//! During file list building and reception, hardlinks are identified by their
//! (device, inode) pairs. This module provides a table structure to track
//! hardlinks and assign unique indices for wire protocol transmission.
//!
//! Uses `FxHashMap` for fast lookups with integer-based keys.
//!
//! # Upstream Reference
//!
//! - `hlink.c:match_hlinkinfo()` - Hardlink matching logic
//! - `hlink.c:init_hard_links()` - Hardlink table initialization
//! - Protocol 30+ uses indices into a hardlink list

mod table;
#[cfg(test)]
mod tests;
mod types;

pub use table::HardlinkTable;
pub use types::{DevIno, HardlinkEntry, HardlinkLookup};
