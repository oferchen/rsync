#![allow(clippy::cast_possible_truncation)]

//! Parser and evaluator for receiver-side `--chmod` modifiers.
//!
//! The upstream rsync CLI allows multiple `--chmod=SPEC` occurrences where each
//! specification may contain comma-separated numeric or symbolic clauses. This
//! module mirrors the clause grammar and applies modifiers to permission modes
//! with behaviour identical to GNU `chmod`, including conditional execute bits
//! and copy directives (for example `g=u`). The [`ChmodModifiers`] type wraps
//! the parsed clauses and exposes [`ChmodModifiers::apply`] so higher layers can
//! evaluate modifiers after the standard metadata preservation step.

mod apply;
mod parse;
mod spec;

use std::fmt;

#[cfg(unix)]
use apply::apply_clauses;
use parse::parse_spec;
use spec::Clause;

/// Error produced when parsing a `--chmod` specification fails.
#[derive(Debug, Eq, PartialEq)]
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

impl fmt::Display for ChmodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ChmodError {}

/// Parsed representation of one or more `--chmod` directives.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ChmodModifiers {
    clauses: Vec<Clause>,
}

impl ChmodModifiers {
    /// Parses a comma-separated chmod specification.
    pub fn parse(spec: &str) -> Result<Self, ChmodError> {
        let clauses = parse_spec(spec)?;
        if clauses.is_empty() {
            return Err(ChmodError::new(
                "chmod specification did not contain any clauses",
            ));
        }
        Ok(Self { clauses })
    }

    /// Appends clauses from another [`ChmodModifiers`] value.
    pub fn extend(&mut self, other: ChmodModifiers) {
        self.clauses.extend(other.clauses);
    }

    /// Applies the modifiers to the provided mode, returning the updated value.
    #[cfg(unix)]
    pub fn apply(&self, mode: u32, file_type: std::fs::FileType) -> u32 {
        apply_clauses(&self.clauses, mode, file_type)
    }

    /// Applies the modifiers on non-Unix platforms.
    #[cfg(not(unix))]
    pub fn apply(&self, mode: u32, _file_type: std::fs::FileType) -> u32 {
        let _ = mode;
        mode
    }

    /// Returns `true` when no clauses are present.
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }
}

#[cfg(test)]
mod tests;
