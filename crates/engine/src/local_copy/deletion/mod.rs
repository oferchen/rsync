//! Deletion decision logic for rsync --delete variants.
//!
//! Determines which destination entries are extraneous (not present in the
//! source) and whether they should be deleted, respecting:
//!
//! - `--delete-excluded`: Also delete files matching exclude patterns
//! - `--max-delete=NUM`: Don't delete more than NUM files
//! - `--force`: Force deletion of non-empty directories
//! - Filter rules and exclusion patterns
//!
//! Deletion timing (`--delete-before`, `--delete-during`, `--delete-after`,
//! `--delete-delay`) is represented by the `DeleteTiming` enum and handled
//! by the caller.

mod strategy;

pub use strategy::{
    DeletionContext, DeletionError, DeletionResult, build_keep_set, is_extraneous_entry,
    should_delete_entry,
};

#[cfg(test)]
mod tests;
