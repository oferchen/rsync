//! Deletion decision logic for rsync --delete variants.
//!
//! Determines which destination entries are extraneous (not present in the
//! source) and whether they should be deleted. `should_delete_entry` checks:
//!
//! - whether deletion is enabled,
//! - the `--max-delete=NUM` limit,
//! - membership in the source entry list,
//! - a caller-supplied filter predicate.
//!
//! `DeletionContext` also carries the `--delete-excluded`, `--force` and
//! `--dry-run` settings; acting on them is left to the caller.
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
