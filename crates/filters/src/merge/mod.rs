//! Merge file reader for filter rules.
//!
//! This module reads filter rules from files in rsync's merge file format.
//! Merge files use the same syntax as `--filter` command-line rules:
//!
//! - `+ PATTERN` or `include PATTERN` - include matching files
//! - `- PATTERN` or `exclude PATTERN` - exclude matching files
//! - `P PATTERN` or `protect PATTERN` - protect from deletion
//! - `R PATTERN` or `risk PATTERN` - remove protection
//! - `. FILE` or `merge FILE` - read additional rules from FILE
//! - `: FILE` or `dir-merge FILE` - read rules per-directory
//! - `!` or `clear` - clear previously defined rules
//! - `H PATTERN` or `hide PATTERN` - sender-only exclude
//! - `S PATTERN` or `show PATTERN` - sender-only include
//!
//! # Modifiers
//!
//! Rules can have modifiers between the action and pattern:
//!
//! - `!` - Negate match (e.g., `-! *.txt` excludes files NOT matching `*.txt`)
//! - `p` - Perishable (ignored during delete-excluded processing)
//! - `s` - Sender-side only
//! - `r` - Receiver-side only
//! - `x` - Xattr filtering only
//! - `e` - Exclude-self (merge rules only: exclude the merge file's own name)
//! - `n` - No-inherit (merge rules only: don't inherit parent rules)
//! - `w` - Word-split (merge rules only: split the merge file at whitespace)
//! - `C` - CVS mode (add CVS exclusion patterns)
//! - `/` - Anchor merged rules to the transfer root (merge rules only)
//! - `-` - Merged lines are literal excludes, prefixes not honoured (merge rules only)
//! - `+` - Merged lines are literal includes, prefixes not honoured (merge rules only)
//!
//! The `e`, `n`, `w`, `/`, `-`, and `+` modifiers are valid only on a merge /
//! dir-merge rule (upstream `FILTRULE_MERGE_FILE`); on any other rule they are a
//! syntax error.
//!
//! Example: `-!p *.tmp` excludes files NOT matching `*.tmp`, marked perishable.
//!
//! Lines starting with `#` or `;` are comments. Empty lines are ignored.
//!
//! # Upstream References
//!
//! - upstream: exclude.c:parse_filter_str() - rule parsing from filter strings
//! - upstream: exclude.c:parse_filter_file() - reading rules from merge files
//! - upstream: exclude.c lines 1220-1288 - modifier character handling

mod error;
pub(crate) mod parse;
pub(crate) mod read;

#[cfg(test)]
mod tests;

pub use error::MergeFileError;
pub use parse::parse_rules;
pub(crate) use read::scope_local_clear;
pub use read::{read_rules, read_rules_recursive};
