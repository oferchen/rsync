//! Deletion strategy implementations for rsync --delete variants.
//!
//! This module provides a unified interface for handling extraneous file
//! deletion at different stages of the transfer process:
//!
//! - `--delete-before`: Remove extraneous files before transfer starts
//! - `--delete-during`: Remove files as each directory is processed (default)
//! - `--delete-after`: Remove files after all transfers complete
//! - `--delete-delay`: Like delete-after but accumulate list during transfer
//!
//! The deletion logic respects:
//! - `--delete-excluded`: Also delete files matching exclude patterns
//! - `--max-delete=NUM`: Don't delete more than NUM files
//! - `--force`: Force deletion of non-empty directories
//! - Filter rules and exclusion patterns

mod strategy;

pub use strategy::{
    DeleteAfterStrategy, DeleteBeforeStrategy, DeleteDelayStrategy, DeleteDuringStrategy,
    DeletionContext, DeletionError, DeletionResult, DeletionStrategy, apply_deletion_strategy,
    build_keep_set, is_extraneous_entry, should_delete_entry,
};

#[cfg(test)]
mod tests;
