#![deny(unsafe_code)]

//! Re-export of shared bandwidth parsing and pacing utilities.
//!
//! The legacy location of the bandwidth helpers lives in this module so
//! existing call sites within `oc_rsync_core` continue to work while the logic has
//! moved into the dedicated [`oc_rsync_bandwidth`] crate.

pub use oc_rsync_bandwidth::*;
