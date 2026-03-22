//! Concurrent delta pipeline types and coordination.
//!
//! This module provides the core data types for dispatching delta computations
//! across threads. [`DeltaWork`] represents a unit of work (one file to
//! transfer), and [`DeltaResult`] captures the outcome including transfer
//! statistics and retry disposition.
//!
//! # Architecture
//!
//! ```text
//! Generator/Receiver
//!       │
//!       ▼
//!   DeltaWork ──► Worker Thread ──► DeltaResult
//!   (file NDX,     (signature        (bytes written,
//!    dest path,     generation,       literal/matched,
//!    basis path)    delta apply)      success/redo/fail)
//! ```
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()` (`receiver.c`)
//! and computes deltas in `match_sums()` (`match.c`). This module parallelizes
//! that work by splitting file processing into discrete [`DeltaWork`] items
//! dispatched to worker threads, with [`DeltaResult`] carrying per-file
//! statistics back for aggregation.
//!
//! # See Also
//!
//! - [`crate::delta`] for block-matching primitives
//! - `transfer::pipeline` for the pipelined receiver architecture

mod types;

pub use types::{DeltaResult, DeltaResultStatus, DeltaWork, DeltaWorkKind};
