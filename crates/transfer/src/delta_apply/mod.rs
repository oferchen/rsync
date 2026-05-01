//! Delta application for file transfer.
//!
//! This module encapsulates the logic for applying delta data received from a sender
//! to reconstruct files. It mirrors upstream rsync's `receive_data()` function from
//! `receiver.c:240`.
//!
//! # Submodules
//!
//! - [`applicator`] - Core delta application logic (`DeltaApplicator`, config, result types)
//! - [`checksum`] - Checksum verification with enum dispatch for algorithm selection
//! - [`sparse`] - Sparse file write state tracking for hole optimization
//!
//! # DEBUG_DELTASUM Tracing Levels
//!
//! This module implements rsync-compatible DEBUG_DELTASUM tracing at 4 levels:
//!
//! - **Level 1**: Basic delta application summary (total stats)
//! - **Level 2**: Token processing milestones (start/end markers)
//! - **Level 3**: Per-token details (literal vs block reference)
//! - **Level 4**: Detailed offset and checksum tracking (very verbose)
//!
//! # Upstream Reference
//!
//! - `receiver.c:240` - `receive_data()` - Main delta application loop
//! - `receiver.c:315` - Token processing loop (literal vs block reference)
//! - `receiver.c:374-382` - Sparse file finalization
//! - `receiver.c:408` - File checksum verification

mod applicator;
mod checksum;
mod sparse;

pub use applicator::{
    BasisWriterKind, DeltaApplicator, DeltaApplyConfig, DeltaApplyResult, apply_delta_stream,
};
pub use checksum::ChecksumVerifier;
pub use sparse::SparseWriteState;
