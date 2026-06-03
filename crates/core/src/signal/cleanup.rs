//! Cleanup coordination for temporary files and resources.
//!
//! This module re-exports [`engine::CleanupManager`] so that both `core` and
//! `transfer` (which cannot depend on `core` due to the circular dependency)
//! share a single process-wide cleanup registry through the `engine` crate.

pub use engine::CleanupManager;
