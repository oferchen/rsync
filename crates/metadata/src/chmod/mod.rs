#![allow(clippy::cast_possible_truncation)]

//! Parser and evaluator for receiver-side `--chmod` modifiers.
//!
//! The upstream rsync CLI allows multiple `--chmod=SPEC` occurrences where each
//! specification may contain comma-separated numeric or symbolic clauses. This
//! module mirrors upstream rsync's `chmod.c:parse_chmod()` grammar exactly,
//! reducing every clause to an AND/OR mask pair (`ModeAND`/`ModeOR`) and the
//! `D`/`F` selectors, then applying them through `chmod.c:tweak_mode()` order:
//! conditional execute bits (`X`), the set-id/sticky bits driven by the who
//! letters, and the umask masking applied to an implied who-class. rsync does
//! not implement GNU-chmod permission-copy forms (`g=u`); those route to a
//! parse error exactly as upstream does. The [`ChmodModifiers`] type wraps the
//! parsed clauses and exposes [`ChmodModifiers::apply`] so higher layers can
//! evaluate modifiers after the standard metadata preservation step.

mod apply;
mod parse;
mod spec;

use thiserror::Error;

#[cfg(unix)]
use apply::apply_clauses;
use parse::parse_spec;
use spec::Clause;

/// Error produced when parsing a `--chmod` specification fails.
#[derive(Debug, Eq, PartialEq, Error)]
#[error("{message}")]
pub struct ChmodError {
    message: String,
}

impl ChmodError {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self {
            message: text.into(),
        }
    }
}

/// Parsed representation of one or more `--chmod` directives.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ChmodModifiers {
    clauses: Vec<Clause>,
}

impl ChmodModifiers {
    /// Parses a comma-separated chmod specification.
    pub fn parse(spec: &str) -> Result<Self, ChmodError> {
        Ok(Self {
            clauses: parse_spec(spec)?,
        })
    }

    /// Appends clauses from another [`ChmodModifiers`] value.
    pub fn extend(&mut self, other: ChmodModifiers) {
        self.clauses.extend(other.clauses);
    }

    /// Applies the modifiers to the provided mode, returning the updated value.
    #[cfg(unix)]
    #[must_use]
    pub fn apply(&self, mode: u32, file_type: std::fs::FileType) -> u32 {
        apply_clauses(&self.clauses, mode, file_type)
    }

    /// Applies the modifiers on non-Unix platforms.
    #[cfg(not(unix))]
    #[must_use]
    pub fn apply(&self, mode: u32, _file_type: std::fs::FileType) -> u32 {
        let _ = mode;
        mode
    }

    /// Returns `true` when no clauses are present.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }
}

#[cfg(test)]
mod tests;
