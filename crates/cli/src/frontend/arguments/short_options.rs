#![deny(unsafe_code)]

//! Expands clusters of short options (e.g. `-avz`) before they reach `clap`,
//! mirroring upstream rsync's parsing semantics.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;

use clap::Command as ClapCommand;

/// Reorders arguments so that recognised options precede positional operands,
/// mirroring upstream rsync's popt-based parsing which accepts options in any
/// position relative to the source and destination paths.
///
/// The CLI front-end declares the trailing operand list with
/// `trailing_var_arg(true)` and `allow_hyphen_values(true)` so it can emit
/// tailored diagnostics for unsupported options (see
/// `execution::operands::extract_operands`). A side effect is that clap routes
/// every token following the first operand into the operand list, so a known
/// option written after the paths (for example `src dst --rsync-path=prog`)
/// would be rejected. Upstream popt processes options regardless of position,
/// so this helper hoists each recognised option - together with its
/// space-separated value - ahead of the operands before clap parses them.
///
/// Tokens after a literal `--` are left untouched, matching popt's end-of-option
/// marker. Unrecognised option-looking tokens are left among the operands so the
/// downstream `extract_operands` check still reports them as unknown options.
pub(crate) fn hoist_options_before_operands(
    command: &ClapCommand,
    args: Vec<OsString>,
) -> Vec<OsString> {
    let mut iter = args.into_iter();
    let Some(program) = iter.next() else {
        return Vec::new();
    };
    let rest: Vec<OsString> = iter.collect();

    let (flag_shorts, value_shorts) = classify_short_options(command);
    let long_options = classify_long_options(command);

    let mut options: Vec<OsString> = Vec::with_capacity(rest.len());
    let mut operands: Vec<OsString> = Vec::with_capacity(rest.len());

    let mut index = 0;
    while index < rest.len() {
        let token = &rest[index];

        if token.as_os_str() == "--" {
            // Preserve the terminator and everything after it verbatim.
            operands.extend(rest[index..].iter().cloned());
            break;
        }

        match classify_token(token, &flag_shorts, &value_shorts, &long_options) {
            TokenKind::Option { needs_space_value } => {
                options.push(token.clone());
                if needs_space_value {
                    if let Some(value) = rest.get(index + 1) {
                        options.push(value.clone());
                        index += 1;
                    }
                }
            }
            TokenKind::Operand => operands.push(token.clone()),
        }

        index += 1;
    }

    let mut result = Vec::with_capacity(options.len() + operands.len() + 1);
    result.push(program);
    result.append(&mut options);
    result.append(&mut operands);
    result
}

/// Classification of a single raw argument token for operand hoisting.
enum TokenKind {
    /// A recognised option; `needs_space_value` marks a value-taking option
    /// written in the space-separated form whose value is the following token.
    Option { needs_space_value: bool },
    /// A positional operand or an unrecognised option-looking token.
    Operand,
}

/// Classifies a raw token as a recognised option or an operand.
fn classify_token(
    token: &OsString,
    flag_shorts: &HashSet<char>,
    value_shorts: &HashSet<char>,
    long_options: &HashMap<String, bool>,
) -> TokenKind {
    let Some(text) = token.to_str() else {
        return TokenKind::Operand;
    };

    if let Some(name) = text.strip_prefix("--") {
        if name.is_empty() {
            return TokenKind::Operand;
        }
        let key = name.split('=').next().unwrap_or(name);
        return match long_options.get(key) {
            Some(true) => TokenKind::Option {
                needs_space_value: !name.contains('='),
            },
            Some(false) => TokenKind::Option {
                needs_space_value: false,
            },
            None => TokenKind::Operand,
        };
    }

    let Some(cluster) = text.strip_prefix('-') else {
        return TokenKind::Operand;
    };

    if cluster.is_empty() {
        // A bare `-` denotes stdin/stdout, not an option.
        return TokenKind::Operand;
    }

    if cluster.chars().count() == 1 {
        let short = cluster.chars().next().expect("one character");
        if value_shorts.contains(&short) {
            return TokenKind::Option {
                needs_space_value: true,
            };
        }
        if flag_shorts.contains(&short) {
            return TokenKind::Option {
                needs_space_value: false,
            };
        }
        return TokenKind::Operand;
    }

    match expand_cluster(text, flag_shorts, value_shorts) {
        Some(fragments) => TokenKind::Option {
            needs_space_value: fragment_needs_value(fragments.last(), value_shorts),
        },
        None => TokenKind::Operand,
    }
}

/// Returns true when the trailing cluster fragment is a bare value-taking short
/// option (for example `-M`), meaning its value is the following argument.
fn fragment_needs_value(fragment: Option<&String>, value_shorts: &HashSet<char>) -> bool {
    let Some(fragment) = fragment else {
        return false;
    };
    let mut chars = fragment.chars();
    if chars.next() != Some('-') {
        return false;
    }
    match (chars.next(), chars.next()) {
        (Some(short), None) => value_shorts.contains(&short),
        _ => false,
    }
}

/// Builds a map of recognised long option names (including visible aliases) to
/// whether the option requires a value.
fn classify_long_options(command: &ClapCommand) -> HashMap<String, bool> {
    let mut long_options = HashMap::new();

    for argument in command.get_arguments() {
        let Some(longs) = argument.get_long_and_visible_aliases() else {
            continue;
        };

        // Honour an explicit `num_args`; otherwise fall back to whether the
        // action consumes a value. Options like `--ssh-identity` use
        // `ArgAction::Append` without an explicit arity, so `get_num_args`
        // returns `None` and must not be read as a valueless flag.
        let requires_value = argument
            .get_num_args()
            .map(|range| range.min_values() > 0)
            .unwrap_or_else(|| argument.get_action().takes_values());

        for long in longs {
            long_options.insert(long.to_owned(), requires_value);
        }
    }

    long_options
}

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

        // Arguments that require at least one value (for example `-M` or
        // `-f<rule>`) must only appear at the end of a cluster so the
        // following argument can be treated as their value. Optional
        // arguments (such as `-h`, which is an `ArgAction::Count` flag) are
        // treated as simple flags; packing a value onto an optional-value
        // short flag is not supported by upstream rsync.
        //
        // Honour an explicit `num_args`; otherwise fall back to whether the
        // action consumes a value, mirroring `classify_long_options`. A
        // value-bearing short with a defaulted arity (for example `-T`, whose
        // Arg sets a `value_parser` but no explicit `num_args`, so its action
        // defaults to `Set`) would otherwise be misclassified as a flag and
        // the packed/`=`-joined forms (`-T/tmp`, `-T=/tmp`) rejected.
        let requires_value = argument
            .get_num_args()
            .map(|range| range.min_values() > 0)
            .unwrap_or_else(|| argument.get_action().takes_values());

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
                let remainder = &cluster[next_offset..];
                // popt (and clap for the un-expanded `-o=value` form) strip a
                // single '=' that joins a short option to its packed value, so
                // `-B=4096` -> `4096` and `-T=/tmp` -> `/tmp`. Mirror that here
                // for every value-bearing short. upstream: popt short-arg parse.
                let value = remainder.strip_prefix('=').unwrap_or(remainder);
                fragments.push(value.to_owned());
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
    use clap::ArgAction;

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
    fn expand_cluster_strips_equals_from_packed_value() {
        // popt strips one '=' joining a short option to its packed value, so
        // `-B=4096` -> `-B 4096` and `-T=/tmp` -> `-T /tmp`. Only one '=' goes.
        let flags = HashSet::new();
        let values = make_flags("BT");
        assert_eq!(
            expand_cluster("-B=4096", &flags, &values),
            Some(vec!["-B".to_owned(), "4096".to_owned()])
        );
        assert_eq!(
            expand_cluster("-T=/tmp", &flags, &values),
            Some(vec!["-T".to_owned(), "/tmp".to_owned()])
        );
        assert_eq!(
            expand_cluster("-T==foo", &flags, &values),
            Some(vec!["-T".to_owned(), "=foo".to_owned()])
        );
        // The attached (no '=') form is untouched.
        assert_eq!(
            expand_cluster("-B4096", &flags, &values),
            Some(vec!["-B".to_owned(), "4096".to_owned()])
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

    #[test]
    fn expand_short_options_value_short_without_explicit_num_args() {
        // Regression: a value-bearing short whose Arg sets a value_parser but no
        // explicit num_args (its action defaults to Set) must still be treated
        // as value-taking, so `-T=/tmp` and `-T/tmp` expand instead of being
        // rejected as unknown options. Mirrors the real `--temp-dir`/`-T` Arg.
        let command = ClapCommand::new("rsync").arg(
            clap::Arg::new("temp-dir")
                .short('T')
                .long("temp-dir")
                .value_parser(clap::builder::OsStringValueParser::new()),
        );
        for (input, value) in [("-T=/tmp", "/tmp"), ("-T/tmp", "/tmp")] {
            let args = vec![OsString::from("rsync"), OsString::from(input)];
            let expanded = expand_short_options(&command, args);
            assert_eq!(
                expanded,
                vec![
                    OsString::from("rsync"),
                    OsString::from("-T"),
                    OsString::from(value),
                ],
                "{input}"
            );
        }
    }

    fn hoist_command() -> ClapCommand {
        ClapCommand::new("rsync")
            .arg(
                clap::Arg::new("archive")
                    .short('a')
                    .long("archive")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                clap::Arg::new("rsync-path")
                    .long("rsync-path")
                    .num_args(1)
                    .action(ArgAction::Set),
            )
            .arg(
                clap::Arg::new("rsh")
                    .short('e')
                    .long("rsh")
                    .num_args(1)
                    .action(ArgAction::Set),
            )
            .arg(
                clap::Arg::new("args")
                    .action(ArgAction::Append)
                    .num_args(0..)
                    .allow_hyphen_values(true)
                    .trailing_var_arg(true),
            )
    }

    fn hoist(args: &[&str]) -> Vec<String> {
        let command = hoist_command();
        let input = args.iter().map(OsString::from).collect();
        hoist_options_before_operands(&command, input)
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn hoist_long_equals_after_operands() {
        assert_eq!(
            hoist(&["rsync", "src/", "dst/", "--rsync-path=/bin/rsync"]),
            vec!["rsync", "--rsync-path=/bin/rsync", "src/", "dst/"]
        );
    }

    #[test]
    fn hoist_long_space_value_after_operands() {
        // The value token following the space-form option is hoisted with it.
        assert_eq!(
            hoist(&["rsync", "src/", "dst/", "--rsync-path", "/bin/rsync"]),
            vec!["rsync", "--rsync-path", "/bin/rsync", "src/", "dst/"]
        );
    }

    #[test]
    fn hoist_short_space_value_after_operands() {
        assert_eq!(
            hoist(&["rsync", "src/", "dst/", "-e", "ssh"]),
            vec!["rsync", "-e", "ssh", "src/", "dst/"]
        );
    }

    #[test]
    fn hoist_leaves_operands_and_order_when_options_lead() {
        assert_eq!(
            hoist(&["rsync", "--archive", "src/", "dst/"]),
            vec!["rsync", "--archive", "src/", "dst/"]
        );
    }

    #[test]
    fn hoist_preserves_double_dash_terminator() {
        // Everything after `--` stays verbatim, even option-looking tokens.
        assert_eq!(
            hoist(&["rsync", "src/", "--", "--rsync-path=/bin/rsync"]),
            vec!["rsync", "src/", "--", "--rsync-path=/bin/rsync"]
        );
    }

    #[test]
    fn hoist_leaves_unknown_option_among_operands() {
        // Unknown options are not hoisted so the downstream operand check can
        // still report them as unsupported.
        assert_eq!(
            hoist(&["rsync", "src/", "dst/", "--bogus"]),
            vec!["rsync", "src/", "dst/", "--bogus"]
        );
    }

    #[test]
    fn hoist_leaves_bare_dash_as_operand() {
        assert_eq!(hoist(&["rsync", "-", "dst/"]), vec!["rsync", "-", "dst/"]);
    }
}
