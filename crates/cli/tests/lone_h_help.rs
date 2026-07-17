//! A bare `-h` prints help, not human-readable output.
//!
//! upstream: options.c:2005 - `human_readable > 1 && argc == 2 && !am_server`
//! preserves the historic meaning of a lone `-h` as `--help`: when the only
//! command-line token increments the human-readable counter, rsync prints usage
//! to stdout and exits 0 instead of failing with a missing-operands error.

fn run(args: &[&str]) -> (i32, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run(args.iter().copied(), &mut stdout, &mut stderr);
    (
        code,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

#[test]
fn lone_dash_h_prints_help_and_exits_zero() {
    let (code, stdout, stderr) = run(&["oc-rsync", "-h"]);
    assert_eq!(code, 0, "a bare -h must exit 0 like --help");
    assert!(
        stdout.contains("Usage:"),
        "help text must go to stdout, got: {stdout}"
    );
    assert!(
        stderr.is_empty(),
        "a bare -h must not emit an error, got: {stderr}"
    );
}

#[test]
fn combined_single_token_with_h_prints_help() {
    // `-avh` is still a single command-line token (argc == 2), and it bumps the
    // human-readable counter, so upstream resolves it to help/exit 0 as well.
    let (code, stdout, stderr) = run(&["oc-rsync", "-avh"]);
    assert_eq!(code, 0, "-avh alone must exit 0");
    assert!(stdout.contains("Usage:"), "help must print for -avh");
    assert!(stderr.is_empty(), "no error expected for -avh: {stderr}");
}

#[test]
fn repeated_h_single_token_prints_help() {
    // `-hh` selects base-1024 units but, as a lone token, still means help.
    let (code, _stdout, stderr) = run(&["oc-rsync", "-hh"]);
    assert_eq!(code, 0, "-hh alone must exit 0");
    assert!(stderr.is_empty(), "no error expected for -hh: {stderr}");
}

#[test]
fn lone_token_without_h_is_not_help() {
    // `-a` alone does NOT bump the human-readable counter, so the historic
    // `-h`-means-help shortcut must not fire: rsync falls through to the
    // missing-operands syntax error (exit 1).
    let (code, _stdout, _stderr) = run(&["oc-rsync", "-a"]);
    assert_eq!(
        code, 1,
        "-a alone has no operands and must exit 1, not print help"
    );
}

#[test]
fn no_human_readable_lone_token_is_not_help() {
    // `--no-human-readable` sets the counter to 0 (not > 1), so it must not be
    // treated as the lone-`-h` help shortcut.
    let (code, _stdout, _stderr) = run(&["oc-rsync", "--no-human-readable"]);
    assert_eq!(
        code, 1,
        "--no-human-readable alone must exit 1, not print help"
    );
}

#[test]
fn two_h_tokens_are_not_help() {
    // `-h -h` is two command-line tokens (argc == 3), so the argc == 2 guard
    // fails and it falls through to the missing-operands error (exit 1).
    let (code, _stdout, _stderr) = run(&["oc-rsync", "-h", "-h"]);
    assert_eq!(
        code, 1,
        "-h -h is two tokens and must exit 1, not print help"
    );
}
