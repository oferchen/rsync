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
//! - `e` - Exclude-only (forces rule to act as exclude)
//! - `n` - No-inherit (for merge rules, don't inherit parent rules)
//! - `w` - Word-split (split pattern on whitespace into multiple rules)
//! - `C` - CVS mode (add CVS exclusion patterns)
//!
//! Example: `-!p *.tmp` excludes files NOT matching `*.tmp`, marked perishable.
//! Example: `-w foo bar baz` creates three exclude rules for "foo", "bar", "baz".
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
pub use read::{read_rules, read_rules_recursive};
