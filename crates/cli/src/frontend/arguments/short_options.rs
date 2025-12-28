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

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== expand_cluster tests ====================

    fn make_flags(chars: &str) -> HashSet<char> {
        chars.chars().collect()
    }

    #[test]
    fn expand_cluster_simple_flags() {
        let flags = make_flags("avz");
        let values = HashSet::new();
        let result = expand_cluster("-avz", &flags, &values);
        assert_eq!(result, Some(vec!["-a".to_string(), "-v".to_string(), "-z".to_string()]));
    }

    #[test]
    fn expand_cluster_single_char_not_expanded() {
        let flags = make_flags("a");
        let values = HashSet::new();
        // Single char options are not expanded (cluster.len() <= 1)
        assert_eq!(expand_cluster("-a", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_not_option() {
        let flags = make_flags("avz");
        let values = HashSet::new();
        // Doesn't start with -
        assert_eq!(expand_cluster("avz", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_long_option() {
        let flags = make_flags("avz");
        let values = HashSet::new();
        // Long options (--) are not expanded
        assert_eq!(expand_cluster("--verbose", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_value_option_at_end() {
        let flags = make_flags("av");
        let values = make_flags("M");
        // Value option at end is valid
        let result = expand_cluster("-avM", &flags, &values);
        assert_eq!(result, Some(vec!["-a".to_string(), "-v".to_string(), "-M".to_string()]));
    }

    #[test]
    fn expand_cluster_value_option_not_at_end() {
        let flags = make_flags("av");
        let values = make_flags("M");
        // Value option not at end is invalid
        assert_eq!(expand_cluster("-aMv", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_unknown_option() {
        let flags = make_flags("av");
        let values = HashSet::new();
        // Unknown option 'x' in cluster
        assert_eq!(expand_cluster("-avx", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_two_flags() {
        let flags = make_flags("rv");
        let values = HashSet::new();
        let result = expand_cluster("-rv", &flags, &values);
        assert_eq!(result, Some(vec!["-r".to_string(), "-v".to_string()]));
    }

    #[test]
    fn expand_cluster_all_value_options() {
        let _flags: HashSet<char> = HashSet::new();
        let values = make_flags("e");
        // Single value option at end is valid (even if it's the only one)
        let result = expand_cluster("-ee", &_flags, &values);
        // First 'e' is a value option but not at end - returns None
        assert_eq!(result, None);
    }

    #[test]
    fn expand_cluster_empty_after_dash() {
        let flags = make_flags("avz");
        let values = HashSet::new();
        // Just "-" with nothing after is not expanded (cluster.len() <= 1)
        assert_eq!(expand_cluster("-", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_mixed_known_unknown() {
        let flags = make_flags("av");
        let values = HashSet::new();
        // 'z' is not known
        assert_eq!(expand_cluster("-avz", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_only_value_option() {
        let flags = HashSet::new();
        let values = make_flags("M");
        // Single value option requires length > 1 to trigger expansion
        // -M has length 1 in cluster, so None
        assert_eq!(expand_cluster("-M", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_multiple_value_options() {
        let flags = HashSet::new();
        let values = make_flags("Me");
        // Two value options - second must be at end
        assert_eq!(expand_cluster("-Me", &flags, &values), None);
    }

    #[test]
    fn expand_cluster_preserves_order() {
        let flags = make_flags("zva");
        let values = HashSet::new();
        let result = expand_cluster("-zva", &flags, &values);
        assert_eq!(result, Some(vec!["-z".to_string(), "-v".to_string(), "-a".to_string()]));
    }
}
