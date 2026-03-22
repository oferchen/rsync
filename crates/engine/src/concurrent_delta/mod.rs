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
//! # Architecture
//!
//! ```text
//! Generator/Receiver
//!       |
//!       v
//!   DeltaWork --> select_strategy() --> DeltaStrategy::process() --> DeltaResult
//!   (file NDX,     (WholeFile or          (literal write or           (bytes written,
//!    dest path,     Delta strategy)         delta apply)               literal/matched,
//!    basis path)                                                       success/redo/fail)
//! ```
//!
//! # See Also
//!
//! - [`crate::delta`] for block-matching primitives
//! - `transfer::pipeline` for the pipelined receiver architecture

pub mod strategy;
mod types;

pub use strategy::{DeltaStrategy, DeltaTransferStrategy, WholeFileStrategy};
pub use types::{DeltaResult, DeltaResultStatus, DeltaWork, DeltaWorkKind};
