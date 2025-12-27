//! crates/core/src/server/shared/mod.rs
//!
//! Shared abstractions used by generator and receiver roles.
//!
//! This module contains common code extracted from generator.rs and receiver.rs
//! to eliminate duplication and provide consistent behavior across both roles.
//!
//! # Modules
//!
//! - [`ChecksumFactory`] - Factory for creating signature algorithms from negotiated parameters

pub mod checksum;

pub use checksum::ChecksumFactory;
