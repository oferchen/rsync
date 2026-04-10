//! Concurrent delta pipeline types and coordination.
//!
//! This module provides the core data types for dispatching delta computations
//! across threads. [`DeltaWork`] represents a unit of work (one file to
//! transfer), and [`DeltaResult`] captures the outcome including transfer
//! statistics and retry disposition.
//!
//! # Strategy Pattern
//!
//! The [`strategy`] submodule applies the Strategy design pattern to work
//! dispatching. [`DeltaStrategy`] is the trait, with [`WholeFileStrategy`] and
//! [`DeltaTransferStrategy`] as concrete implementations. Use
//! [`strategy::select_strategy`] to obtain the correct strategy for a work
//! item, or [`strategy::dispatch`] as a one-call convenience.
//!
//! The [`work_queue`] submodule provides a bounded channel that limits
//! in-flight work items to prevent OOM when transferring millions of files.
//! The producer blocks when the queue is full, applying backpressure to the
//! generator. Consumers drain items in parallel via `rayon::scope`.
//!
//! The [`reorder`] submodule provides [`ReorderBuffer`] - a sequence-based
//! reorder buffer that restores sequential ordering after parallel dispatch.
//! Each [`DeltaResult`] carries a `sequence` number assigned before dispatch;
//! the consumer feeds results into the buffer and drains them in order.
//!
//! # Architecture
//!
//! ```text
//! Generator/Receiver
//!       |
//!       v
//!   DeltaWork --> WorkQueue (bounded) --> select_strategy() --> DeltaStrategy::process() --> DeltaResult
//!   (file NDX,     backpressure when       (WholeFile or          (literal write or           (bytes written,
//!    dest path,     queue is full            Delta strategy)         delta apply)               sequence,
//!    basis path)                                                                               literal/matched,
//!                                                                                              success/redo/fail)
//!       --> ReorderBuffer --> consumer (in-order)
//!            (BTreeMap,        post-processing sees
//!             capacity bound)  files in submission order
//! ```
//!
//! # See Also
//!
//! - [`crate::delta`] for block-matching primitives
//! - `transfer::pipeline` for the pipelined receiver architecture

pub mod reorder;
pub mod strategy;
mod types;
pub mod work_queue;

pub use reorder::ReorderBuffer;
pub use strategy::{DeltaStrategy, DeltaTransferStrategy, WholeFileStrategy};
pub use types::{DeltaResult, DeltaResultStatus, DeltaWork, DeltaWorkKind};
