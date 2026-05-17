//! Internal utilities shared across engine submodules.
//!
//! Currently exposes [`poison`] for recovering from lock poisoning on
//! synchronization primitives whose protected state remains valid after a
//! panic.

pub mod poison;
