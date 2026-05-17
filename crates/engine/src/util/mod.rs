//! Internal utilities shared across engine subsystems.
//!
//! Helpers live here when they do not fit a single feature area. Keep this
//! module narrow: it is for primitives reused by multiple submodules, not a
//! catch-all.

pub mod arc_diag;

pub use arc_diag::try_unwrap_or_log;
