#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Checksum primitives mirroring upstream rsync algorithms.
//!
//! The crate exposes the rolling weak checksum (`rsum`) together with strong
//! digests backed by MD4, MD5, and XXH64. These building blocks are wired into
//! higher layers by the `core` facade once the transfer engine is implemented.

mod rolling;
pub mod strong;

pub use rolling::{RollingChecksum, RollingDigest, RollingError};
