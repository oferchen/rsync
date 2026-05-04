//! Source enumeration, metadata resolution, and copy dispatch.
//!
//! Decomposes the source processing pipeline into focused submodules:
//!
//! - [`types`] - Data types for destination state and source processing context.
//! - [`destination`] - Destination state queries and target path computation.
//! - [`metadata`] - Source metadata fetching, symlink resolution, relative paths.
//! - [`handlers`] - File-type-specific copy handlers (directory, symlink, FIFO, device).
//! - [`orchestration`] - Top-level source iteration, deferred operations, error rollback.

mod destination;
mod handlers;
mod metadata;
mod orchestration;
mod types;

pub(crate) use orchestration::copy_sources;
