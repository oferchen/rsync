//! Per-directory merge file (`.rsync-filter`) parsing and loading.
//!
//! Provides recursive loaders for filter files that follow upstream rsync's
//! `dir-merge` semantics, including modifier handling, list-clearing, and
//! propagation of `clear_inherited` to parent scopes.

mod load;
mod parse;

pub(crate) use load::{
    NestedDirMerge, apply_dir_merge_rule_defaults, filter_program_local_error,
    load_dir_merge_rules_recursive, resolve_dir_merge_path,
};
pub(crate) use parse::{FilterParseError, ParsedFilterDirective, parse_filter_directive_line};
