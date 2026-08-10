//! Telemetry types for [`super::RegisteredBufferGroup`].
//!
//! Re-exported from [`crate::io_uring_common`] so the registry impl can publish
//! a single concrete type across both the synchronous group and the shared-ring
//! pool. Keeping the definitions in `io_uring_common` avoids a cyclic dependency
//! when the stub crate needs the same shapes on non-Linux targets.

pub use crate::io_uring_common::{RegisteredBufferStats, RegisteredBufferStatus};
