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
//! - `hlink.c:idev_find()` - per-device (dev, ino) lookup used by the sender
//! - `hlink.c:init_hard_links()` - hardlink table initialization
//! - `flist.c:send_file_entry()` - protocol 30+ sends the follower's leader as a
//!   file-list index (`first_hlink_ndx`)

mod table;
#[cfg(test)]
mod tests;
mod trace;
mod types;

pub use table::HardlinkTable;
pub use trace::{
    trace_found_flist_match, trace_hashtable_for_dev, trace_leader_is, trace_looking_for_leader,
    trace_virtual_first, trace_waiting_for,
};
pub use types::{DevIno, HardlinkEntry, HardlinkLookup};
