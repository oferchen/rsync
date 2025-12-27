//! crates/core/src/server/shared/mod.rs
//!
//! Shared abstractions used by generator and receiver roles.
//!
//! This module contains common code extracted from generator.rs and receiver.rs
//! to eliminate duplication and provide consistent behavior across both roles.
//!
//! # Modules
//!
//! - [`checksum`] - Checksum factory for creating signature algorithms

pub mod checksum;

pub use checksum::ChecksumFactory;
