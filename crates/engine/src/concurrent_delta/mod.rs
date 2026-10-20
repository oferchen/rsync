//! Per-file delta work items and the bounded work queue that carries them.
//!
//! [`DeltaWork`] describes one file's receive-side work (NDX, paths, size,
//! whole-file vs delta) and [`DeltaResult`] its outcome. The [`work_queue`]
//! submodule provides the bounded, backpressured channel the drum-buffer-rope
//! throughput governor sizes (`crate::throughput`): producers block when the
//! queue is full rather than spinning.
//!
//! The parallel receive-side apply pipeline that once consumed these types was
//! removed: it was never wired into the receiver, measured 1.7-3.3x slower than
//! the sequential path in isolation, and offered no end-to-end gain because the
//! receiver is not on the critical path. Upstream rsync applies deltas
//! sequentially in `receiver.c:recv_files()`, which the production receiver
//! mirrors.

mod types;
pub mod work_queue;

pub use types::{DeltaResult, DeltaResultStatus, DeltaWork, DeltaWorkKind, FileNdx};
