#![deny(unsafe_code)]

//! Re-export of shared bandwidth parsing and pacing utilities.
//!
//! The legacy location of the bandwidth helpers lives in this module so
//! existing call sites within `core` continue to work while the logic has
//! moved into the dedicated [`bandwidth`] crate.

pub use bandwidth::*;
