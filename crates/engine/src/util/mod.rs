//! Internal utilities shared across engine submodules.
//!
//! Exposes [`self::poison`] for recovering from lock poisoning on
//! synchronization primitives whose protected state remains valid after a
//! panic, and [`self::cleanup`] for global temp-file cleanup coordination.

pub mod cleanup;
pub mod poison;
