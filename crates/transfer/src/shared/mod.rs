//! Shared abstractions used by generator and receiver roles.
//!
//! This module contains common code extracted from generator.rs and receiver.rs
//! to eliminate duplication and provide consistent behavior across both roles.
//!
//! # Modules
//!
//! - [`ChecksumFactory`] - Factory for creating signature algorithms from negotiated parameters
//! - [`TransferDeadline`] - Monotonic deadline for `--stop-at` / `--stop-after` enforcement

pub mod checksum;
pub mod deadline;

pub use checksum::ChecksumFactory;
pub use deadline::TransferDeadline;
