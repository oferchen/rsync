//! Change-set tracking for local-copy operations.
//!
//! `LocalCopyChangeSet` captures which file attributes changed during a
//! local copy, and `TimeChange` describes how the modification time was
//! adjusted. Together they feed the itemize output that mirrors upstream
//! rsync's `log.c` behaviour.

mod detection;
mod types;

pub use types::{LocalCopyChangeSet, TimeChange};

#[cfg(test)]
mod tests;
