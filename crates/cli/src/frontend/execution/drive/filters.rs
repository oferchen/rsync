#![deny(unsafe_code)]

use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use core::client::{ClientConfigBuilder, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use core::message::Message;
use logging_sink::MessageSink;

use super::messages::fail_with_message;
use crate::frontend::filter_rules::{
    FilterDirective, FilterOrderToken, append_apple_double_exclude_rules, append_cvs_exclude_rules,
    append_filter_rules_from_files, apply_merge_directive, cvs_default_exclude_rules,
    merge_directive_options, os_string_to_pattern, parse_filter_directive, parse_old_prefix_rule,
};

/// Filter configuration supplied by the command line.
///
/// The ordered token stream preserves the argv position of every
/// filter-producing option so evaluation is first-match-wins over encounter
/// order, mirroring upstream options.c.
pub(crate) struct FilterInputs {
    pub(crate) order: Vec<FilterOrderToken>,
}

/// Applies CLI-provided filter rules to the [`ClientConfigBuilder`].
///
/// Rules are appended in command-line order so the filter engine's
/// first-match-wins evaluation matches upstream rsync, where each
/// `--include`/`--exclude`/`--filter`/`--include-from`/`--exclude-from`/`-C`/
/// `-F` is fed to the rule list at its argv position (exclude.c:parse_filter_str
/// appends in encounter order).
pub(crate) fn apply_filters<Err>(
    mut builder: ClientConfigBuilder,
    inputs: FilterInputs,
    stderr: &mut MessageSink<Err>,
) -> Result<ClientConfigBuilder, i32>
where
    Err: std::io::Write,
{
    let mut filter_rules = Vec::new();
    let mut merge_stack = HashSet::new();
    let merge_base = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    for token in inputs.order {
        let result = match token {
            FilterOrderToken::Include(pattern) => {
                push_old_prefix_rule(&mut filter_rules, pattern, FilterRuleKind::Include)
            }
            FilterOrderToken::Exclude(pattern) => {
                push_old_prefix_rule(&mut filter_rules, pattern, FilterRuleKind::Exclude)
            }
            FilterOrderToken::IncludeFrom(file) => append_filter_rules_from_files(
                &mut filter_rules,
                std::slice::from_ref(&file),
                FilterRuleKind::Include,
            ),
            FilterOrderToken::ExcludeFrom(file) => append_filter_rules_from_files(
                &mut filter_rules,
                std::slice::from_ref(&file),
                FilterRuleKind::Exclude,
            ),
            FilterOrderToken::Filter(rule) => push_filter_directive(
                &mut filter_rules,
                &rule,
                merge_base.as_path(),
                &mut merge_stack,
            ),
            FilterOrderToken::CvsExclude => append_cvs_exclude_rules(&mut filter_rules),
            FilterOrderToken::AppleDoubleSkip => {
                append_apple_double_exclude_rules(&mut filter_rules)
            }
        };
        if let Err(message) = result {
            return Err(fail_with_message(message, stderr));
        }
    }

    if !filter_rules.is_empty() {
        builder = builder.extend_filter_rules(filter_rules);
    }

    Ok(builder)
}

/// Parses one `--filter`/`-f`/`-F` directive and appends its rules.
fn push_filter_directive(
    filter_rules: &mut Vec<FilterRuleSpec>,
    rule: &OsString,
    merge_base: &std::path::Path,
    merge_stack: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    match parse_filter_directive(rule.as_os_str())? {
        FilterDirective::Rule(spec) => filter_rules.push(spec),
        FilterDirective::Merge(directive) => {
            let effective_options =
                merge_directive_options(&DirMergeOptions::default(), &directive);
            let directive = directive.with_options(effective_options);
            apply_merge_directive(directive, merge_base, filter_rules, merge_stack)?;
        }
        FilterDirective::Clear => filter_rules.clear(),
        FilterDirective::CvsDefaults => filter_rules.extend(cvs_default_exclude_rules()?),
    }
    Ok(())
}

/// Adds a CLI-supplied `--exclude`/`--include` pattern to `destination`,
/// honoring upstream's `XFLG_OLD_PREFIXES` compatibility mode.
///
/// upstream: options.c:1512-1519 routes `--exclude`/`--include` through
/// `parse_filter_str(..., XFLG_OLD_PREFIXES)`, so a value of `- pat` (or
/// `+ pat`) flips the rule kind and `!` clears the list. Plain patterns
/// retain `default_kind`.
fn push_old_prefix_rule(
    destination: &mut Vec<FilterRuleSpec>,
    pattern: OsString,
    default_kind: FilterRuleKind,
) -> Result<(), Message> {
    let text = os_string_to_pattern(pattern);
    match parse_old_prefix_rule(&text, default_kind)? {
        FilterDirective::Rule(rule) => destination.push(rule),
        FilterDirective::Clear => destination.push(FilterRuleSpec::clear()),
        FilterDirective::Merge(_) | FilterDirective::CvsDefaults => {
            unreachable!("parse_old_prefix_rule only emits Rule or Clear")
        }
    }
    Ok(())
}
