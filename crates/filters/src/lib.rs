#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `filters` provides ordered include/exclude/protect pattern evaluation for the
//! Rust `rsync` workspace. The implementation focuses on reproducing the
//! subset of rsync's filter grammar that governs `--include`/`--exclude`
//! handling for local filesystem transfers. Patterns honour anchored matches
//! (leading `/`), directory-only rules (trailing `/`), and recursive wildcards
//! using the same glob semantics exposed by upstream rsync. Rules are evaluated
//! sequentially with the last matching include/exclude directive determining
//! whether a path is copied. `protect` directives accumulate alongside these
//! rules to prevent matching destination paths from being removed during
//! `--delete` sweeps.
//!
//! # Design
//!
//! - [`FilterRule`] captures the user-supplied action (`Include`/`Exclude`/
//!   `Protect`) and pattern text. The rule itself is lightweight; heavy lifting
//!   happens when a [`FilterSet`] is constructed.
//! - [`FilterSet`] owns the compiled representation of each rule, expanding
//!   directory-only patterns into matchers that also cover their contents while
//!   deduplicating equivalent glob expressions. Protect rules are tracked in a
//!   dedicated list so deletion checks can honour them without affecting copy
//!   decisions.
//! - Matching occurs against relative paths using native [`std::path::Path`] semantics so
//!   callers can operate directly on `std::path::PathBuf` instances without
//!   additional conversions.
//!
//! # Invariants
//!
//! - Include/exclude rules are applied in definition order. The last matching
//!   rule wins and defaults to `Include` when no rule matches.
//! - Trailing `/` marks a directory-only rule. The directory itself must match
//!   the rule to trigger exclusion; descendants are excluded automatically.
//! - Leading `/` anchors a rule to the transfer root. Patterns without a leading
//!   slash are matched at any depth by implicitly prefixing `**/`.
//! - Protect rules accumulate independently of include/exclude decisions and
//!   prevent matching destination paths from being removed when `--delete` is
//!   active.
//!
//! # Errors
//!
//! [`FilterSet::from_rules`] reports [`FilterError`] when a rule expands to an
//! invalid glob expression. The error includes the offending pattern and the
//! underlying [`globset::Error`] for debugging.
//!
//! # Examples
//!
//! Build a filter list that excludes editor swap files while explicitly
//! re-including a tracked directory:
//!
//! ```
//! use filters::{FilterRule, FilterSet};
//! use std::path::Path;
//!
//! let rules = [
//!     FilterRule::exclude("*.swp"),
//!     FilterRule::exclude("*.tmp"),
//!     FilterRule::include("important/"),
//! ];
//! let filters = FilterSet::from_rules(rules).expect("filters compile");
//!
//! assert!(filters.allows(Path::new("notes.txt"), false));
//! assert!(filters.allows(Path::new("important/report.txt"), false));
//! assert!(!filters.allows(Path::new("scratch.swp"), false));
//! ```
//!
//! # See also
//!
//! - `engine::local_copy` integrates [`FilterSet`] to prune directory
//!   traversals during deterministic local copies.
//! - [`globset`] for the glob matching primitives used internally.

mod action;
mod compiled;
pub mod cvs;
mod decision;
mod error;
pub mod merge;
mod rule;
mod set;

pub use action::FilterAction;
pub use cvs::{DEFAULT_CVSIGNORE, default_patterns as cvs_default_patterns};
pub use error::FilterError;
pub use merge::{MergeFileError, parse_rules, read_rules, read_rules_recursive};
pub use rule::FilterRule;
pub use set::{FilterSet, FilterSetError, cvs_exclusion_rules};

#[cfg(test)]
mod tests;
