//! Unit tests for the popt-style server argv rewrite.

use super::long_options::LONG_OPTIONS;
use super::{
    ArgvRefusal, FILTER_OPTIONS, LongOption, is_two_arg_server_long_flag, normalize_server_argv,
};
use std::ffi::OsString;

fn argv(tokens: &[&str]) -> Vec<OsString> {
    tokens.iter().map(OsString::from).collect()
}

fn normalized(tokens: &[&str]) -> Vec<String> {
    normalize_server_argv(&argv(tokens))
        .expect("accepted")
        .into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect()
}

/// Every short cluster, before `--server` or after it, joins one flag
/// string placed right after `--server`, as popt's single pass reads them.
#[test]
fn clusters_anywhere_join_one_flag_string() {
    // Ahead of `--server` a letter is parsed twice (options.c:1517-1524).
    assert_eq!(
        normalized(&["rsync", "-v", "--server", "-re.iLsfxCIvu", ".", "d/"]),
        ["rsync", "--server", "-vvre.iLsfxCIvu", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-r", ".", "-t", "d/"]),
        ["rsync", "--server", "-rt", ".", "d/"]
    );
}

/// The rewrite of an upstream server argv is the identity, so it is safe
/// to run twice (the secluded-args list goes through it again).
#[test]
fn upstream_server_argv_is_unchanged() {
    let upstream = [
        "rsync",
        "--server",
        "-logDtpre.iLsfxCIvu",
        "-B2048",
        "-@-1",
        "--delete",
        "--temp-dir",
        "/t",
        ".",
        "d/",
    ];
    assert_eq!(normalized(&upstream), upstream);
}

/// A value-taking letter ends the cluster; its value is the rest of the
/// token or the next token.
#[test]
fn value_options_take_the_rest_or_the_next_token() {
    assert_eq!(
        normalized(&["rsync", "-rM--x", "--server", ".", "d/"]),
        ["rsync", "--server", "-rr", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "-B", "2048", "-T", "/t", "--server", ".", "d/"]),
        ["rsync", "-B2048", "--temp-dir=/t", "--server", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-re", ".iLs", ".", "d/"]),
        ["rsync", "--server", "-re.iLs", ".", "d/"]
    );
}

/// Filter options join their values behind the flag string in argv order;
/// those ahead of `--server` count twice (options.c:1517-1524 restarts the
/// pass without clearing `filter_list` or `F_option_cnt`).
#[test]
fn filter_options_are_joined_and_doubled_ahead_of_server() {
    assert_eq!(
        normalized(&["rsync", "-f", "- *.tmp", "--server", "-r", ".", "d/"]),
        [
            "rsync",
            "--server",
            "-r",
            "--filter=- *.tmp",
            "--filter=- *.tmp",
            ".",
            "d/"
        ]
    );
    assert_eq!(
        normalized(&[
            "rsync",
            "--server",
            "-rF",
            "--exclude",
            "b",
            "--include=a",
            ".",
            "d/"
        ]),
        [
            "rsync",
            "--server",
            "-r",
            "--filter=: /.rsync-filter",
            "--exclude=b",
            "--include=a",
            ".",
            "d/"
        ]
    );
    // One `-F` ahead of `--server` is two, and a third adds nothing.
    assert_eq!(
        normalized(&["rsync", "-F", "--server", "-rF", ".", "d/"]),
        [
            "rsync",
            "--server",
            "-r",
            "--filter=: /.rsync-filter",
            "--filter=- .rsync-filter",
            ".",
            "d/"
        ]
    );
    let once = normalized(&["rsync", "-f", "- x", "--server", "-r", ".", "d/"]);
    let once: Vec<OsString> = once.into_iter().map(OsString::from).collect();
    assert_eq!(normalize_server_argv(&once).unwrap(), once);
}

/// popt's refusals, worded and attributed as 3.5.1 prints them.
#[test]
fn refusals_follow_upstream_text_and_side() {
    let refuse = |tokens: &[&str]| normalize_server_argv(&argv(tokens)).unwrap_err();
    assert_eq!(
        refuse(&["rsync", "-vj", "--server", ".", "d/"]),
        ArgvRefusal {
            text: "-vj: unknown option".to_owned(),
            after_server: false,
        }
    );
    assert_eq!(
        refuse(&["rsync", "--server", "-j", ".", "d/"]),
        ArgvRefusal {
            text: "on remote machine: -j: unknown option".to_owned(),
            after_server: true,
        }
    );
    assert_eq!(
        refuse(&["rsync", "-Mfoo", "--server", ".", "d/"]),
        ArgvRefusal {
            text: "Remote option must start with a dash: foo".to_owned(),
            after_server: false,
        }
    );
}

/// Tokens after `--` are operands, whatever they look like.
#[test]
fn operands_after_the_marker_are_untouched() {
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--", ".", "-j"]),
        ["rsync", "--server", "-r", "--", ".", "-j"]
    );
}

/// The table rows of one kind, each with its letter where it has one.
fn rows(wanted: fn(LongOption) -> Option<char>) -> Vec<(&'static str, char)> {
    LONG_OPTIONS
        .iter()
        .filter_map(|&(name, kind)| wanted(kind).map(|letter| (name, letter)))
        .collect()
}

/// A long name of a short switch is that switch: popt sets the same
/// variable, so the server must read `-M--perms` exactly as `-M-p`.
#[test]
fn every_long_name_of_a_switch_joins_the_flag_string() {
    let letters = rows(|kind| match kind {
        LongOption::Letter(letter) => Some(letter),
        _ => None,
    });
    assert_eq!(letters.len(), 40);
    for (name, letter) in letters {
        let option = format!("--{name}");
        assert_eq!(
            normalized(&["rsync", "--server", "-r", &option, ".", "d/"]),
            ["rsync", "--server", &format!("-r{letter}"), ".", "d/"],
            "{option}"
        );
    }
}

/// A `--no-` negation resets its variable, so every earlier occurrence of
/// the letter stops counting while later ones still do.
#[test]
fn every_negation_clears_its_letter() {
    let negations = rows(|kind| match kind {
        LongOption::Clears(letter) => Some(letter),
        _ => None,
    });
    assert_eq!(negations.len(), 51);
    for (name, letter) in negations {
        let option = format!("--{name}");
        let cluster = format!("-{letter}r{letter}");
        let later = format!("-{letter}");
        assert_eq!(
            normalized(&["rsync", "--server", &cluster, &option, &later, ".", "d/"]),
            ["rsync", "--server", &format!("-r{letter}"), ".", "d/"],
            "{option}"
        );
    }
}

/// upstream: options.c:1686-1702 - `-a` sets each of its variables, so a
/// negation of one leaves the others set.
#[test]
fn a_negation_after_archive_keeps_the_rest_of_it() {
    assert_eq!(
        normalized(&["rsync", "--server", "-av", "--no-p", ".", "d/"]),
        ["rsync", "--server", "-rltgoDv", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-av", "--no-v", ".", "d/"]),
        ["rsync", "--server", "-a", ".", "d/"]
    );
}

/// popt binds a value-taking option's value from the next token; the rewrite
/// joins the two so no value is read as an operand.
#[test]
fn every_split_value_option_is_joined() {
    let values = rows(|kind| (kind == LongOption::Value).then_some('='));
    assert_eq!(values.len(), 58);
    for (name, _) in values {
        let option = format!("--{name}");
        if FILTER_OPTIONS.contains(&option.as_str())
            || is_two_arg_server_long_flag(&option)
            || name == "remote-option"
            || name == "chown"
        {
            continue;
        }
        assert_eq!(
            normalized(&["rsync", "--server", "-r", &option, "V", ".", "d/"]),
            ["rsync", "--server", "-r", &format!("{option}=V"), ".", "d/"],
            "{option}"
        );
    }
}

/// `--block-size` and `--rsh` are the `-B` and `-e` entries' long names.
#[test]
fn value_letters_are_read_as_their_short_options() {
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--block-size=2048", ".", "d/"]),
        ["rsync", "--server", "-r", "-B2048", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-re.iLs", "--rsh", "x", ".", "d/"]),
        ["rsync", "--server", "-rex", ".", "d/"]
    );
}

/// upstream: options.c:1961-2000 - `--chown` is `--usermap` / `--groupmap`
/// of `*` plus `-o` / `-g` for each non-empty half.
#[test]
fn chown_becomes_the_maps_it_implies() {
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--chown=u:g", ".", "d/"]),
        [
            "rsync",
            "--server",
            "-rog",
            "--usermap=*:u",
            "--groupmap=*:g",
            ".",
            "d/"
        ]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--chown", ":g", ".", "d/"]),
        ["rsync", "--server", "-rg", "--groupmap=*:g", ".", "d/"]
    );
}

/// A switch with a joined value stays for the long-option pass to refuse,
/// and a value-taking option with no value left is popt's refusal.
#[test]
fn malformed_long_options_follow_popt() {
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--perms=1", ".", "d/"]),
        ["rsync", "--server", "-r", "--perms=1", ".", "d/"]
    );
    assert_eq!(
        normalize_server_argv(&argv(&["rsync", "--server", "-r", "--log-file"])).unwrap_err(),
        ArgvRefusal {
            text: "on remote machine: --log-file: missing argument".to_owned(),
            after_server: true,
        }
    );
}

/// The rewrite stays idempotent with long options in it, as the secluded-args
/// list needs.
#[test]
fn long_option_rewrite_is_idempotent() {
    let once = normalized(&[
        "rsync",
        "--server",
        "-av",
        "--no-p",
        "--perms",
        "--block-size",
        "1024",
        "--chown=u:g",
        "--log-file-format",
        "%o",
        ".",
        "d/",
    ]);
    let once: Vec<OsString> = once.into_iter().map(OsString::from).collect();
    assert_eq!(normalize_server_argv(&once).unwrap(), once);
}
