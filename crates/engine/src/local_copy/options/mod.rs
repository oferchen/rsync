//! Configuration structures and helpers for local copy execution.
//!
//! [`LocalCopyOptions`] gathers all behavioural flags that influence how a
//! [`LocalCopyPlan`](super::LocalCopyPlan) is executed. Construct instances
//! through the [`LocalCopyOptionsBuilder`] returned by
//! [`LocalCopyOptions::builder()`].

mod backup;
mod batch;
mod builder;
mod compression;
mod deletion;
mod filters;
mod integrity;
mod limits;
mod link_dest;
mod logging;
mod metadata;
mod path_behavior;
mod staging;
mod types;

pub use builder::{BuilderError, LocalCopyOptionsBuilder};
pub use types::{DeleteTiming, LocalCopyOptions, ReferenceDirectory, ReferenceDirectoryKind};
