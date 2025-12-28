#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `delta` module hosts helpers that mirror upstream rsync's block-matching
//! heuristics. The [`calculate_signature_layout`] function replicates the
//! "square root" block-size calculation performed in `generator.c:sum_sizes_sqroot()`
//! (rsync 3.4.1). Future delta-transfer stages reuse this information when
//! computing rolling and strong checksums for individual blocks.
//!
//! # Design
//!
//! Layout types and functions are re-exported from the [`signature`] crate.
//! Delta-specific functionality is re-exported from the [`matching`] crate:
//! [`DeltaGenerator`] for generating delta tokens, [`DeltaSignatureIndex`] for
//! fast signature lookups, and [`DeltaScript`]/[`DeltaToken`] for representing
//! and applying delta streams.
//!
//! # See also
//!
//! - [`crate::local_copy`] will integrate these helpers as the delta-transfer
//!   pipeline evolves.
//! - [`DeltaGenerator`] exposes the delta-token generator that complements the
//!   layout helpers.
//! - [`apply_delta`] applies delta streams to recreate target payloads.
//! - Upstream `generator.c::sum_sizes_sqroot()` for the reference C
//!   implementation mirrored by the signature crate.

// Re-export delta types from the matching crate for backward compatibility.
// The matching crate is now the source of truth for these types.
pub use matching::{
    DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta, generate_delta,
};

// Re-export layout types from the signature crate for backward compatibility.
// The signature crate is now the source of truth for these types.
pub use signature::{
    SignatureLayout, SignatureLayoutError, SignatureLayoutParams, calculate_signature_layout,
};
