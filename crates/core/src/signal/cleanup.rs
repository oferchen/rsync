//! Cleanup coordination for temporary files and resources.
//!
//! Re-exports [`engine::CleanupManager`] so that both `core` and `transfer`
//! share a single global registry. The canonical implementation lives in
//! `engine::util::cleanup`.

pub use engine::CleanupManager;
