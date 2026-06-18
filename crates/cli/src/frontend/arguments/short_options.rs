#![deny(unsafe_code)]

//! Expands clusters of short options (e.g. `-avz`) before they reach `clap`,
//! mirroring upstream rsync's parsing semantics.

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
/// parser.
///
/// Tokens that contain an option requiring a value may only place that option
/// at the end of the cluster to match upstream semantics. When such an option
/// has additional characters after it (for example `-f:C`, where `f` is the
/// short alias for `--filter` and `:C` is the packed value), the remainder is
/// split off as a separate argument so clap can pair it with the option as the
/// value. This matches upstream rsync's handling of packed short-arg values
/// and is required for filter spellings like `-f-C` and `-f:C` that contain
/// characters clap would otherwise misinterpret as another short option.
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
        // Arguments that require `=` separator (such as `-h` with
        // `require_equals(true)` + `num_args(0..=1)`) do not consume a packed
        // remainder. clap routes only `-h=2` to the option; `-hh` clusters two
        // independent `-h` occurrences. Treat them as flag-like so the cluster
        // expander emits one `-h` per character and bare flag counts work.
        let require_equals_with_optional_value =
            argument.is_require_equals_set() && min_values == 0;

        // Arguments that require at least one value (for example `-M`) must
        // only appear at the end of a cluster so the following argument can be
        // treated as their value. Optional arguments (such as `-h`) are
        // treated as simple flags to preserve existing clap behaviour.
        let requires_value = !require_equals_with_optional_value
            && (min_values > 0
                || (takes_values && matches!(action, ArgAction::Set | ArgAction::Append)));

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

    for (offset, short) in cluster.char_indices() {
        if value_options.contains(&short) {
            fragments.push(format!("-{short}"));
            // Upstream rsync treats any characters after a value-bearing short
            // option as the packed value (for example `-f:C` becomes `-f` with
            // value `:C`). Emit the remainder as a separate token so clap pairs
            // it with the option, instead of misparsing characters like `:` or
            // `-` as additional short options.
            let next_offset = offset + short.len_utf8();
            if next_offset < cluster.len() {
                fragments.push(cluster[next_offset..].to_owned());
            }
            return Some(fragments);
        } else if flag_options.contains(&short) {
            fragments.push(format!("-{short}"));
        } else {
            return None;
        }
    }

    Some(fragments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_flags(chars: &str) -> HashSet<char> {
        chars.chars().collect()
    }

    #[test]
    fn expand_cluster_simple_flags() {
        let flags = make_flags("avz");
        let values = HashSet::new();
        let result = expand_cluster("-avz", &flags, &values);
        assert_eq!(
            result,
            Some(vec!["-a".to_owned(), "-v".to_owned(), "-z".to_owned()])
        );
    }

    #[test]
    fn expand_cluster_single_char_not_expanded() {
        let flags = make_flags("a");
        let values = HashSet::new();
        assert_eq!(expand_cluster("-a", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_not_option() {
        let flags = make_flags("avz");
        let values = HashSet::new();
        assert_eq!(expand_cluster("avz", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_long_option() {
        let flags = make_flags("avz");
        let values = HashSet::new();
        assert_eq!(expand_cluster("--verbose", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_value_option_at_end() {
        let flags = make_flags("av");
        let values = make_flags("M");
        let result = expand_cluster("-avM", &flags, &values);
        assert_eq!(
            result,
            Some(vec!["-a".to_owned(), "-v".to_owned(), "-M".to_owned()])
        );
    }

    #[test]
    fn expand_cluster_value_option_not_at_end() {
        let flags = make_flags("av");
        let values = make_flags("M");
        // Characters after the value option become the packed value, matching
        // upstream rsync's `-T<dir>` / `-f<rule>` behaviour.
        let result = expand_cluster("-aMv", &flags, &values);
        assert_eq!(
            result,
            Some(vec!["-a".to_owned(), "-M".to_owned(), "v".to_owned()])
        );
    }

    #[test]
    fn expand_cluster_unknown_option() {
        let flags = make_flags("av");
        let values = HashSet::new();
        assert_eq!(expand_cluster("-avx", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_two_flags() {
        let flags = make_flags("rv");
        let values = HashSet::new();
        let result = expand_cluster("-rv", &flags, &values);
        assert_eq!(result, Some(vec!["-r".to_owned(), "-v".to_owned()]));
    }

    #[test]
    fn expand_cluster_all_value_options() {
        // `-ee` packs `e` as the value of the first `-e`, mirroring upstream's
        // packed short-arg form (`-eVALUE`).
        let _flags: HashSet<char> = HashSet::new();
        let values = make_flags("e");
        let result = expand_cluster("-ee", &_flags, &values);
        assert_eq!(result, Some(vec!["-e".to_owned(), "e".to_owned()]));
    }

    #[test]
    fn expand_cluster_empty_after_dash() {
        let flags = make_flags("avz");
        let values = HashSet::new();
        assert_eq!(expand_cluster("-", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_mixed_known_unknown() {
        let flags = make_flags("av");
        let values = HashSet::new();
        assert_eq!(expand_cluster("-avz", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_only_value_option() {
        let flags = HashSet::new();
        let values = make_flags("M");
        assert_eq!(expand_cluster("-M", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_multiple_value_options() {
        // `-Me` is `-M` with packed value `e`.
        let flags = HashSet::new();
        let values = make_flags("Me");
        let result = expand_cluster("-Me", &flags, &values);
        assert_eq!(result, Some(vec!["-M".to_owned(), "e".to_owned()]));
    }

    #[test]
    fn expand_cluster_preserves_order() {
        let flags = make_flags("zva");
        let values = HashSet::new();
        let result = expand_cluster("-zva", &flags, &values);
        assert_eq!(
            result,
            Some(vec!["-z".to_owned(), "-v".to_owned(), "-a".to_owned()])
        );
    }

    #[test]
    fn expand_cluster_filter_packed_colon_rule() {
        // upstream rsync `testsuite/exclude.test` uses `-f:C` to inject the
        // `:C` (dir-merge .cvsignore) rule via the packed short-arg form.
        let flags = HashSet::new();
        let values = make_flags("f");
        let result = expand_cluster("-f:C", &flags, &values);
        assert_eq!(result, Some(vec!["-f".to_owned(), ":C".to_owned()]));
    }

    #[test]
    fn expand_cluster_filter_packed_dash_rule() {
        // `-f-C` packs the `-C` (cvs-style exclude) rule. The remainder retains
        // its leading dash so the rule parser sees the original spelling.
        let flags = HashSet::new();
        let values = make_flags("f");
        let result = expand_cluster("-f-C", &flags, &values);
        assert_eq!(result, Some(vec!["-f".to_owned(), "-C".to_owned()]));
    }

    #[test]
    fn expand_cluster_value_after_flag_prefix() {
        // Mirrors `-avf:C`: archive and verbose are flags, filter is the
        // trailing value option, and `:C` is its packed value.
        let flags = make_flags("av");
        let values = make_flags("f");
        let result = expand_cluster("-avf:C", &flags, &values);
        assert_eq!(
            result,
            Some(vec![
                "-a".to_owned(),
                "-v".to_owned(),
                "-f".to_owned(),
                ":C".to_owned(),
            ])
        );
    }

    #[test]
    fn expand_short_options_filter_packed_colon_rule() {
        // End-to-end check that the public entry point recognises `-f:C` once
        // the filter Arg is wired as a value-bearing short.
        let command = ClapCommand::new("rsync").arg(
            clap::Arg::new("filter")
                .short('f')
                .long("filter")
                .action(ArgAction::Append)
                .num_args(1),
        );
        let args = vec![OsString::from("rsync"), OsString::from("-f:C")];
        let expanded = expand_short_options(&command, args);
        assert_eq!(
            expanded,
            vec![
                OsString::from("rsync"),
                OsString::from("-f"),
                OsString::from(":C"),
            ]
        );
    }

    #[test]
    fn expand_short_options_filter_packed_dash_rule() {
        let command = ClapCommand::new("rsync").arg(
            clap::Arg::new("filter")
                .short('f')
                .long("filter")
                .action(ArgAction::Append)
                .num_args(1),
        );
        let args = vec![OsString::from("rsync"), OsString::from("-f-C")];
        let expanded = expand_short_options(&command, args);
        assert_eq!(
            expanded,
            vec![
                OsString::from("rsync"),
                OsString::from("-f"),
                OsString::from("-C"),
            ]
        );
    }
}
