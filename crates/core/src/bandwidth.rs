#![deny(unsafe_code)]

//! Re-export of shared bandwidth parsing and pacing utilities.
//!
//! The legacy location of the bandwidth helpers lives in this module so
//! existing call sites within `rsync_core` continue to work while the logic has
//! moved into the dedicated [`rsync_bandwidth`] crate.

pub use rsync_bandwidth::*;
