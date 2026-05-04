//! Plan construction and execution types for local filesystem copies.
//!
//! The central type is [`LocalCopyPlan`], which is constructed from CLI-style
//! operands and executed to produce a [`LocalCopySummary`] or a detailed
//! [`LocalCopyReport`]. Execution emits [`LocalCopyRecord`] values describing
//! each filesystem action, and callers can observe them in real time through
//! the [`LocalCopyRecordHandler`] trait.

mod action;
mod change_set;
mod execution;
mod metadata;
mod plan_impl;
mod progress;
mod record;
mod report;
mod summary;

pub use action::LocalCopyAction;
pub use change_set::{LocalCopyChangeSet, TimeChange};
pub use execution::LocalCopyExecution;
pub use metadata::{LocalCopyFileKind, LocalCopyMetadata};
pub use plan_impl::LocalCopyPlan;
pub use progress::LocalCopyProgress;
pub use record::{LocalCopyRecord, LocalCopyRecordHandler};
pub use report::LocalCopyReport;
pub use summary::LocalCopySummary;

#[cfg(test)]
pub(crate) use super::filter_program::FilterOutcome;
