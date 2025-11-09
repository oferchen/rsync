#![deny(unsafe_code)]

use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use rsync_core::client::{ClientConfigBuilder, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use rsync_logging::MessageSink;

use super::messages::fail_with_message;
use crate::frontend::filter_rules::{
    FilterDirective, append_cvs_exclude_rules, append_filter_rules_from_files,
    apply_merge_directive, merge_directive_options, os_string_to_pattern, parse_filter_directive,
};

/// Filter configuration supplied by the command line.
pub(crate) struct FilterInputs {
    pub(crate) exclude_from: Vec<OsString>,
    pub(crate) include_from: Vec<OsString>,
    pub(crate) excludes: Vec<OsString>,
    pub(crate) includes: Vec<OsString>,
    pub(crate) filters: Vec<OsString>,
    pub(crate) cvs_exclude: bool,
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
        &inputs.exclude_from,
        FilterRuleKind::Exclude,
    ) {
        return Err(fail_with_message(message, stderr));
    }

    filter_rules.extend(
        inputs
            .excludes
            .into_iter()
            .map(|pattern| FilterRuleSpec::exclude(os_string_to_pattern(pattern))),
    );

    if let Err(message) = append_filter_rules_from_files(
        &mut filter_rules,
        &inputs.include_from,
        FilterRuleKind::Include,
    ) {
        return Err(fail_with_message(message, stderr));
    }

    filter_rules.extend(
        inputs
            .includes
            .into_iter()
            .map(|pattern| FilterRuleSpec::include(os_string_to_pattern(pattern))),
    );

    if inputs.cvs_exclude {
        if let Err(message) = append_cvs_exclude_rules(&mut filter_rules) {
            return Err(fail_with_message(message, stderr));
        }
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
            Err(message) => return Err(fail_with_message(message, stderr)),
        }
    }

    if !filter_rules.is_empty() {
        builder = builder.extend_filter_rules(filter_rules);
    }

    Ok(builder)
}
