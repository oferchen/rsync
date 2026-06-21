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
    FilterDirective, append_apple_double_exclude_rules, append_cvs_exclude_rules,
    append_filter_rules_from_files, apply_merge_directive, cvs_default_exclude_rules,
    merge_directive_options, os_string_to_pattern, parse_filter_directive, parse_old_prefix_rule,
};

/// Filter configuration supplied by the command line.
pub(crate) struct FilterInputs {
    pub(crate) exclude_from: Vec<OsString>,
    pub(crate) include_from: Vec<OsString>,
    pub(crate) excludes: Vec<OsString>,
    pub(crate) includes: Vec<OsString>,
    pub(crate) filters: Vec<OsString>,
    pub(crate) cvs_exclude: bool,
    pub(crate) apple_double_skip: bool,
}

/// Applies CLI-provided filter rules to the [`ClientConfigBuilder`].
pub(crate) fn apply_filters<Err>(
    mut builder: ClientConfigBuilder,
    inputs: FilterInputs,
    stderr: &mut MessageSink<Err>,
) -> Result<ClientConfigBuilder, i32>
where
    Err: std::io::Write,
{
    let mut filter_rules = Vec::new();
    if let Err(message) = append_filter_rules_from_files(
        &mut filter_rules,
        &inputs.include_from,
        FilterRuleKind::Include,
    ) {
        return Err(fail_with_message(message, stderr));
    }

    for pattern in inputs.includes {
        if let Err(message) =
            push_old_prefix_rule(&mut filter_rules, pattern, FilterRuleKind::Include)
        {
            return Err(fail_with_message(message, stderr));
        }
    }

    if let Err(message) = append_filter_rules_from_files(
        &mut filter_rules,
        &inputs.exclude_from,
        FilterRuleKind::Exclude,
    ) {
        return Err(fail_with_message(message, stderr));
    }

    for pattern in inputs.excludes {
        if let Err(message) =
            push_old_prefix_rule(&mut filter_rules, pattern, FilterRuleKind::Exclude)
        {
            return Err(fail_with_message(message, stderr));
        }
    }

    if inputs.cvs_exclude
        && let Err(message) = append_cvs_exclude_rules(&mut filter_rules)
    {
        return Err(fail_with_message(message, stderr));
    }

    if inputs.apple_double_skip
        && let Err(message) = append_apple_double_exclude_rules(&mut filter_rules)
    {
        return Err(fail_with_message(message, stderr));
    }

    let mut merge_stack = HashSet::new();
    let merge_base = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for filter in &inputs.filters {
        match parse_filter_directive(filter.as_os_str()) {
            Ok(FilterDirective::Rule(spec)) => filter_rules.push(spec),
            Ok(FilterDirective::Merge(directive)) => {
                let effective_options =
                    merge_directive_options(&DirMergeOptions::default(), &directive);
                let directive = directive.with_options(effective_options);
                if let Err(message) = apply_merge_directive(
                    directive,
                    merge_base.as_path(),
                    &mut filter_rules,
                    &mut merge_stack,
                ) {
                    return Err(fail_with_message(message, stderr));
                }
            }
            Ok(FilterDirective::Clear) => filter_rules.clear(),
            Ok(FilterDirective::CvsDefaults) => match cvs_default_exclude_rules() {
                Ok(rules) => filter_rules.extend(rules),
                Err(message) => return Err(fail_with_message(message, stderr)),
            },
            Err(message) => return Err(fail_with_message(message, stderr)),
        }
    }

    if !filter_rules.is_empty() {
        builder = builder.extend_filter_rules(filter_rules);
    }

    Ok(builder)
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
