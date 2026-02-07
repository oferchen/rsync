#![deny(unsafe_code)]

//! Block matching and delta generation for rsync transfers.
//!
//! This crate provides the rsync delta algorithm implementation:
//! - [`DeltaGenerator`] generates delta tokens by comparing input against a signature
//! - [`DeltaSignatureIndex`] indexes signatures for fast block lookup
//! - [`DeltaScript`] and [`DeltaToken`] represent delta streams
//! - [`FuzzyMatcher`] finds similar basis files for delta transfers
//!
//! # Design
//!
//! The delta algorithm reuses the rolling checksum from the `checksums` crate
//! and signature types from the `signature` crate. The generator produces a
//! stream of copy and literal tokens that reconstruct a target file from a
//! basis file plus the transmitted delta.
//!
//! # See also
//!
//! - [`signature`] crate for signature generation
//! - Upstream `match.c` for the C implementation this module mirrors

mod fuzzy;
mod generator;
mod index;
pub mod optimized_search;
mod ring_buffer;
mod script;

pub use fuzzy::{
    FuzzyMatch, FuzzyMatcher, FUZZY_LEVEL_1, FUZZY_LEVEL_2, compute_similarity_score,
};
pub use generator::{DeltaGenerator, generate_delta};
pub use index::DeltaSignatureIndex;
pub use script::{DeltaScript, DeltaToken, apply_delta};
