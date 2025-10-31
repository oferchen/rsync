//! Utilities for parsing and loading filter rules supplied via the CLI.

mod arguments;
mod cvs;
mod directive;
mod merge;
mod parsing;
mod sources;

pub(crate) use arguments::{collect_filter_arguments, locate_filter_arguments};
pub(crate) use cvs::append_cvs_exclude_rules;
pub(crate) use directive::{FilterDirective, merge_directive_options, os_string_to_pattern};
pub(crate) use merge::apply_merge_directive;
pub(crate) use parsing::parse_filter_directive;
pub(crate) use sources::append_filter_rules_from_files;

#[cfg(test)]
pub(crate) use directive::MergeDirective;

#[cfg(test)]
pub(crate) use merge::process_merge_directive;

#[cfg(test)]
pub(crate) use parsing::parse_merge_modifiers;

#[cfg(test)]
pub(crate) use sources::load_filter_file_patterns;

#[cfg(test)]
pub(crate) use sources::set_filter_stdin_input;
