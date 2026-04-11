//! Concurrent delta computation pipeline for parallel file processing.
//!
//! Parallelizes the per-file receive loop that upstream rsync executes
//! sequentially in `receiver.c:recv_files()`. Each file becomes a [`DeltaWork`]
//! item dispatched through a bounded [`work_queue`] to rayon worker threads,
//! producing a [`DeltaResult`] that is reordered for in-sequence delivery.
//!
//! # Components
//!
//! | Submodule | Type | Role |
//! |-----------|------|------|
//! | `types` | [`DeltaWork`], [`DeltaResult`] | Per-file work item (NDX, paths, size) and outcome (stats, redo/fail status) |
//! | [`work_queue`] | `WorkQueueSender` / `WorkQueueReceiver` | Bounded `sync_channel` (2x thread count) with backpressure |
//! | [`strategy`] | [`DeltaStrategy`] trait | Dispatches to [`WholeFileStrategy`] or [`DeltaTransferStrategy`] based on [`DeltaWorkKind`] |
//! | [`reorder`] | [`ReorderBuffer`] | `BTreeMap`-backed buffer that yields results in submission order |
//! | [`consumer`] | [`DeltaConsumer`] | Background thread that drains `WorkQueue` via `drain_parallel` into `ReorderBuffer` for in-order delivery |
//!
//! # Production Pipeline
//!
//! ```text
//! Receiver (producer thread)
//!   |  assigns monotonic sequence number
//!   |  creates DeltaWork (whole-file or delta)
//!   v
//! WorkQueue ──bounded channel──► rayon::scope workers
//!   blocks when full               |
//!   (backpressure to receiver)      |  select_strategy(&work)
//!                                   |  strategy.process(&work)
//!                                   v
//!                              DeltaResult (with sequence stamp)
//!                                   |
//!                                   v
//!                              ReorderBuffer
//!                                   |  drain_ready() yields contiguous run
//!                                   v
//!                              Consumer (in file-list order)
//!                                   - checksum verification
//!                                   - temp-file commit / rename
//!                                   - metadata application
//!                                   - redo collection for phase 2
//! ```
//!
//! The bounded queue caps in-flight items at `2 * rayon::current_num_threads()`,
//! preventing OOM for million-file transfers. The reorder buffer ensures
//! post-processing sees files in the same order as upstream's sequential loop.
//!
//! # Upstream Reference
//!
//! - `receiver.c:recv_files()` - sequential per-file loop this module parallelizes
//! - `receiver.c:receive_data()` - literal vs delta data path (`fd2 == -1` selects whole-file)
//!
//! # See Also
//!
//! - [`crate::delta`] for block-matching primitives
//! - `transfer::pipeline` for the pipelined receiver architecture

pub mod consumer;
pub mod reorder;
pub mod strategy;
mod types;
pub mod work_queue;

pub use consumer::DeltaConsumer;
pub use reorder::ReorderBuffer;
pub use strategy::{DeltaStrategy, DeltaTransferStrategy, WholeFileStrategy};
pub use types::{DeltaResult, DeltaResultStatus, DeltaWork, DeltaWorkKind};
