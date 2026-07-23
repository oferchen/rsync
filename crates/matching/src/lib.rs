#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(not(test), warn(clippy::unwrap_used))]

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
/// Two-level block match search mirroring upstream rsync's `match.c`.
pub mod optimized_search;
mod ring_buffer;
mod script;

pub use fuzzy::{
    FUZZY_LEVEL_1, FUZZY_LEVEL_2, FuzzyMatch, FuzzyMatcher, trace_fuzzy_basis_selected,
    trace_fuzzy_distance, trace_fuzzy_size_mtime_match,
};
pub use generator::{DeltaGenerator, generate_delta};
pub use index::{
    DeltaSignatureIndex, HASH_KEY_BITS, HashtableRole, MatchedBlocks, trace_hashtable_created,
    trace_hashtable_destroyed, trace_hashtable_growing,
};
pub use script::{DeltaScript, DeltaToken, apply_delta};
