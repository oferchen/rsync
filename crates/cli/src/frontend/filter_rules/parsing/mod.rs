//! Filter-rule line parsing, decomposed by concern.
//!
//! The public entry points live in [`entry`]; non-merge rule forms in
//! [`rules`]; the verbose `merge` / `dir-merge` directives in [`directives`].
//! Tokenizing, modifier, shorthand, and short-merge helpers remain in their
//! existing sibling modules ([`helpers`], [`modifiers`], [`shorthand`],
//! [`merge`]).

mod directives;
mod entry;
mod helpers;
mod merge;
mod modifiers;
mod rule_line;
mod rules;
mod shorthand;

#[cfg(test)]
mod tests;

pub(crate) use entry::{parse_filter_directive, parse_old_prefix_rule};

/// Argument-sourced entry point for `parse_merge_modifiers`, used by the
/// frontend's test shim. Production callers inside this module pass the
/// `RuleLine` they are already parsing, so the provenance travels with the
/// text; only a directive the operator typed can be re-paired here.
#[cfg(test)]
pub(crate) fn parse_merge_modifiers_from_argument(
    modifiers: &str,
    directive: &str,
    is_dir_merge: bool,
) -> Result<(core::client::DirMergeOptions, bool), core::message::Message> {
    merge::parse_merge_modifiers(
        modifiers,
        rule_line::RuleLine::new(directive, filters::RuleSource::Argument),
        is_dir_merge,
    )
}

// Brought into the hub so the `tests` submodule's `use super::*` resolves the
// same names that were in scope when the tests lived alongside the parsers.
#[cfg(test)]
use std::ffi::OsStr;

#[cfg(test)]
use core::client::{FilterRuleKind, FilterRuleSpec};

#[cfg(test)]
use super::directive::FilterDirective;

#[cfg(test)]
use directives::{parse_dir_merge_alias, parse_long_merge_directive};
#[cfg(test)]
use entry::{is_cvs_convenience_rule, parse_rule_directive};
#[cfg(test)]
use rules::{
    parse_exclude_if_present, parse_keyword_rule, parse_short_include_rule, parse_shorthand_rules,
};
