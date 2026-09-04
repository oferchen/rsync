#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(not(test), warn(clippy::unwrap_used))]

//! # Overview
//!
//! `filters` provides ordered include/exclude/protect pattern evaluation for the
//! Rust `rsync` workspace, implementing the Chain of Responsibility pattern.
//! The implementation reproduces rsync's filter grammar that governs
//! `--include`/`--exclude`/`--filter` handling. Patterns honour anchored matches
//! (leading `/`), directory-only rules (trailing `/`), and recursive wildcards
//! using the same glob semantics exposed by upstream rsync. Rules are evaluated
//! sequentially with the first matching include/exclude directive determining
//! whether a path is copied. `protect` directives accumulate alongside these
//! rules to prevent matching destination paths from being removed during
//! `--delete` sweeps.
//!
//! # Chain of Responsibility
//!
//! Filter evaluation follows the Chain of Responsibility pattern: each compiled
//! rule is a handler in an ordered chain. When a path is evaluated, rules are
//! tested in definition order and the first matching rule determines the
//! outcome (first-match-wins). If no rule matches, the default action is to
//! include the path. This mirrors upstream rsync's `check_filter()` in
//! `exclude.c`, which iterates from the head of the filter list and returns on
//! the first match.
//!
//! Two independent chains are maintained:
//!
//! 1. **Include/Exclude chain** - governs whether a path is transferred.
//! 2. **Protect/Risk chain** - governs whether a path may be deleted on the
//!    receiver when `--delete` is active.
//!
//! # Design
//!
//! - [`FilterRule`] captures the user-supplied action (`Include`/`Exclude`/
//!   `Protect`/`Risk`/`Clear`/`Merge`/`DirMerge`) and pattern text. The rule
//!   itself is lightweight; heavy lifting happens when a [`FilterSet`] is
//!   constructed.
//! - [`FilterSet`] owns the compiled representation of each rule, expanding
//!   directory-only patterns into matchers that also cover their contents while
//!   deduplicating equivalent glob expressions. Protect rules are tracked in a
//!   dedicated list so deletion checks can honour them without affecting copy
//!   decisions.
//! - Matching occurs against relative paths using native [`std::path::Path`] semantics so
//!   callers can operate directly on `std::path::PathBuf` instances without
//!   additional conversions.
//!
//! # Upstream References
//!
//! - `exclude.c` - filter rule parsing, compilation, and evaluation
//! - `exclude.c:check_filter()` - first-match-wins evaluation loop
//! - `exclude.c:parse_filter_str()` - rule parsing from `--filter` strings
//! - `rsync.1` - man page documentation for filter rules
//!
//! # Invariants
//!
//! - Include/exclude rules are applied in definition order. The first matching
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
/// AppleDouble (`._foo`) sidecar exclusion patterns for `--apple-double-skip`.
pub mod apple_double;
/// Per-directory scoped filter chain with push/pop semantics.
pub mod chain;
/// Lexical `..` collapse for filter paths, mirroring `clean_fname()`.
pub mod clean_fname;
pub mod clear_token;
mod compiled;
/// CVS exclusion patterns for rsync's `--cvs-exclude` (`-C`) option.
pub mod cvs;
/// Structured tracing for filter rule evaluation and statistics.
pub mod debug_filter;
mod decision;
mod error;
/// Implied-include validation of received file-list names (CVE-2022-29154).
pub mod implied;
/// Merge-file reader and parser for filter rules.
pub mod merge;
mod merge_open;
mod rule;
pub mod rule_source;
/// Traversal clamp for peer- and file-supplied names, mirroring
/// `sanitize_path()`.
///
/// Lives in the same crate as [`clean_fname`] because both port the same
/// upstream file (`util1.c`) and answer the same question about a path, and
/// because this is the one crate every consumer of the rule can reach: the CLI
/// clamps `--files-from` lines, the daemon clamps peer-requested module paths,
/// and the transfer generator clamps names arriving over the wire. Keeping one
/// owner is what stops the two dialects drifting - they decide different things
/// and are not interchangeable: `clean_fname` KEEPS a `..` it cannot consume,
/// while `sanitize_path` DROPS it once the depth budget is spent.
pub mod sanitize_path;
mod set;
mod wildmatch;

pub use action::FilterAction;
pub use apple_double::{
    DEFAULT_APPLE_DOUBLE_PATTERN, default_patterns as apple_double_default_patterns,
};
pub use chain::{DirFilterGuard, DirMergeConfig, FilterChain, FilterChainError};
pub use clean_fname::collapse_dot_dot_dirs;
pub use clear_token::{ClearToken, classify_clear_token};
pub use compiled::XattrSide;
pub use cvs::{DEFAULT_CVSIGNORE, default_patterns as cvs_default_patterns};
pub use error::FilterError;
pub use implied::{ImpliedIncludeOptions, ImpliedIncludes};
pub use merge::{
    MAX_MERGE_DEPTH, MergeFileError, depth_limit_exceeded, parse_rules, read_rules,
    read_rules_recursive,
};
pub use rule::FilterRule;
pub use rule_source::{RuleSource, trace_add_rule};
pub use set::{FilterSet, FilterSetError, apple_double_exclusion_rules, cvs_exclusion_rules};
pub use wildmatch::{iwildmatch, wildmatch};

#[cfg(test)]
mod tests;
