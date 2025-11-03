#![deny(unsafe_code)]

use std::collections::HashSet;
use std::ffi::OsString;

use clap::builder::ValueRange;
use clap::{ArgAction, Command as ClapCommand};

/// Expands clusters of short options so the [`clap`] command can parse them.
///
/// Upstream rsync treats a token such as `-avz` as the concatenation of
/// `-a`, `-v`, and `-z` as long as every option in the cluster is a recognised
/// short flag. The CLI front-end asks clap to accept hyphen-prefixed operands
/// unchanged so it can emit tailored diagnostics for unsupported options. That
/// behaviour prevents clap from splitting short-option clusters on its own,
/// so this helper performs the expansion before handing the arguments to the
/// parser. Tokens that contain an option requiring a value may only place that
/// option at the end of the cluster to match upstream semantics.
pub(crate) fn expand_short_options(command: &ClapCommand, args: Vec<OsString>) -> Vec<OsString> {
    let (flag_options, value_options) = classify_short_options(command);

    let mut expanded = Vec::with_capacity(args.len());
    let mut after_double_dash = false;

    for argument in args {
        if after_double_dash {
            expanded.push(argument);
            continue;
        }

        if argument.as_os_str() == "--" {
            after_double_dash = true;
            expanded.push(argument);
            continue;
        }

        let Some(text) = argument.to_str() else {
            expanded.push(argument);
            continue;
        };

        if let Some(cluster) = expand_cluster(text, &flag_options, &value_options) {
            expanded.extend(cluster.into_iter().map(OsString::from));
        } else {
            expanded.push(argument);
        }
    }

    expanded
}

fn classify_short_options(command: &ClapCommand) -> (HashSet<char>, HashSet<char>) {
    let mut flag_options = HashSet::new();
    let mut value_options = HashSet::new();

    for argument in command.get_arguments() {
        let Some(shorts) = argument.get_short_and_visible_aliases() else {
            continue;
        };

        let num_args = argument.get_num_args().unwrap_or(ValueRange::EMPTY);
        let takes_values = num_args.takes_values();
        let min_values = num_args.min_values();
        let action = argument.get_action();

        // Arguments that require at least one value (for example `-M`) must
        // only appear at the end of a cluster so the following argument can be
        // treated as their value. Optional arguments (such as `-h`) are
        // treated as simple flags to preserve existing clap behaviour.
        let requires_value = min_values > 0
            || (takes_values && matches!(action, ArgAction::Set | ArgAction::Append));

        if requires_value {
            value_options.extend(shorts);
        } else {
            flag_options.extend(shorts);
        }
    }

    (flag_options, value_options)
}

fn expand_cluster(
    text: &str,
    flag_options: &HashSet<char>,
    value_options: &HashSet<char>,
) -> Option<Vec<String>> {
    if !text.starts_with('-') || text.starts_with("--") {
        return None;
    }

    let cluster = &text[1..];
    if cluster.len() <= 1 {
        return None;
    }

    let mut fragments = Vec::with_capacity(cluster.len());
    let mut chars = cluster.chars().peekable();

    while let Some(short) = chars.next() {
        if value_options.contains(&short) {
            if chars.peek().is_some() {
                return None;
            }
            fragments.push(format!("-{short}"));
        } else if flag_options.contains(&short) {
            fragments.push(format!("-{short}"));
        } else {
            return None;
        }
    }

    Some(fragments)
}
