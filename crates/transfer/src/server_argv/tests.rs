//! Unit tests for the popt-style server argv rewrite.

use super::{ArgvRefusal, normalize_server_argv};
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
