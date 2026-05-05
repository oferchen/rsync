//! Filter directive parser for per-directory merge files.
//!
//! Splits the `.rsync-filter` grammar into focused submodules: `line`
//! dispatches a single line to the appropriate handler, `merge` and
//! `dir_merge` parse the corresponding directives, `modifiers` shares the
//! short and keyword modifier logic, and `types` carries the public AST.

mod dir_merge;
mod line;
mod merge;
mod modifiers;
mod types;

pub(crate) use line::parse_filter_directive_line;
pub(crate) use types::{FilterParseError, ParsedFilterDirective};
