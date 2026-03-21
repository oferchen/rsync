#![deny(unsafe_code)]

//! Implements upstream rsync's `--dry-run` (`-n`) output format.
//!
//! The dry-run feature simulates a transfer without actually performing it,
//! showing what would be transferred, deleted, or modified. This module provides
//! types and formatters for displaying planned actions in a format that matches
//! upstream rsync.

mod action;
mod format;
mod formatter;
mod summary;

#[cfg(test)]
mod tests;

pub use action::DryRunAction;
pub use format::format_number_with_commas;
pub use formatter::DryRunFormatter;
pub use summary::DryRunSummary;
