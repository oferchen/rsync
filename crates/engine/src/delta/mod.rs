#![allow(clippy::module_name_repetitions)]

//! Block-matching helpers for the delta-transfer pipeline.
//!
//! Re-exports the signature-layout primitives from [`signature`] and the
//! delta-generation/application types from [`matching`]. The signature side
//! mirrors upstream `generator.c:sum_sizes_sqroot()` (rsync 3.4.1) for block
//! size and strong-checksum length selection; the matching side produces and
//! consumes the resulting token streams.
//!
//! See [`crate::local_copy`] for the higher-level transfer driver that wires
//! these helpers into actual file copies.

/// Delta generation, application, and signature-lookup types.
///
/// - [`DeltaGenerator`] streams [`DeltaToken`]s by matching the sender's basis
///   blocks against the receiver's signatures.
/// - [`DeltaSignatureIndex`] provides O(1) rolling-checksum lookups during
///   match scanning.
/// - [`DeltaScript`] aggregates tokens for replay; [`apply_delta`] reconstructs
///   the target file from a script plus the basis.
/// - [`generate_delta`] is the high-level convenience wrapper around
///   [`DeltaGenerator`].
pub use matching::{
    DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta, generate_delta,
};

/// Signature-layout primitives mirroring upstream `generator.c:sum_sizes_sqroot()`.
///
/// [`calculate_signature_layout`] picks block size and strong-checksum length
/// from the file size using the upstream square-root heuristic.
/// [`SignatureLayoutParams`] captures the inputs; [`SignatureLayout`] carries
/// the resolved layout; [`SignatureLayoutError`] reports invalid inputs.
pub use signature::{
    SignatureLayout, SignatureLayoutError, SignatureLayoutParams, calculate_signature_layout,
};
