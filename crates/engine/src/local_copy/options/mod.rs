//! Configuration structures and helpers for local copy execution.

mod backup;
mod batch;
mod compression;
mod deletion;
mod filters;
mod integrity;
mod limits;
mod link_dest;
mod metadata;
mod path_behavior;
mod staging;
mod types;

pub use types::{DeleteTiming, LocalCopyOptions, ReferenceDirectory, ReferenceDirectoryKind};
