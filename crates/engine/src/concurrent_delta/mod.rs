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
//!       |
//!       v
//!   DeltaWork --> Worker Thread --> DeltaResult
//!   (file NDX,     (signature        (bytes written,
//!    dest path,     generation,       literal/matched,
//!    basis path)    delta apply)      success/redo/fail)
//! ```
//!
//! # See Also
//!
//! - [`crate::delta`] for block-matching primitives
//! - `transfer::pipeline` for the pipelined receiver architecture

mod types;

pub use types::{DeltaResult, DeltaResultStatus, DeltaWork, DeltaWorkKind};
