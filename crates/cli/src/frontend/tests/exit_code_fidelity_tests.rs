//! Exit-code fidelity with upstream rsync 3.4.4 for argument-validation
//! failures. Each case is verified against the real upstream binary.

use super::common::*;
use super::*;
use tempfile::tempdir;

/// upstream: compat.c:190 `unknown compress name` -> RERR_UNSUPPORTED
/// (errcode.h:28 `RERR_UNSUPPORTED 4`), not RERR_SYNTAX.
#[test]
fn invalid_compress_choice_returns_unsupported() {
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-choice=invalid"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    assert_eq!(code, 4);
}

/// An empty `--compress-choice=` is likewise an unusable name (exit 4).
#[test]
fn empty_compress_choice_returns_unsupported() {
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-choice="),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    assert_eq!(code, 4);
}

/// upstream: options.c:2031-2034 - `--compress-choice=auto` is nulled out so
/// normal codec negotiation runs; it is accepted (exit 0), not rejected.
#[test]
fn compress_choice_auto_is_accepted() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-choice=auto"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);
    assert_eq!(code, 0, "--compress-choice=auto should be accepted");
    assert!(destination.exists());
}

/// upstream: options.c:2031 only special-cases the exact token `auto`;
/// `--compress-choice=auto,auto` is an unknown compress name (exit 4).
#[test]
fn compress_choice_auto_auto_returns_unsupported() {
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-choice=auto,auto"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    assert_eq!(code, 4);
}

/// upstream: options.c:2483-2489 - with `--files-from`, more than two operands
/// (or a lone operand) is a syntax error (RERR_SYNTAX = 1).
#[test]
fn files_from_extra_operands_return_syntax_error() {
    let temp = tempdir().expect("tempdir");
    let list = temp.path().join("list");
    std::fs::write(&list, b"file\n").expect("write list");
    let mut list_arg = OsString::from("--files-from=");
    list_arg.push(list.as_os_str());

    // Three operands with --files-from.
    let (three, _o, _e) = run_with_args([
        OsString::from(RSYNC),
        list_arg.clone(),
        OsString::from("a"),
        OsString::from("b"),
        OsString::from("c"),
    ]);
    assert_eq!(three, 1, "three operands with --files-from should exit 1");

    // A lone operand (missing destination) with --files-from.
    let (one, _o, _e) = run_with_args([OsString::from(RSYNC), list_arg, OsString::from("a")]);
    assert_eq!(one, 1, "lone operand with --files-from should exit 1");
}

/// upstream: checksum.c:139 `unknown checksum name` -> RERR_UNSUPPORTED (4).
#[test]
fn invalid_checksum_choice_returns_unsupported() {
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--checksum-choice=invalid"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    assert_eq!(code, 4);
}

/// An empty `--checksum-choice=` is likewise an unusable name (exit 4).
#[test]
fn empty_checksum_choice_returns_unsupported() {
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--checksum-choice="),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    assert_eq!(code, 4);
}

/// upstream: exclude.c:1107 parse_rule_tok returns NULL for an empty rule, so a
/// blank `--filter` value is a no-op and the transfer succeeds (exit 0).
#[test]
fn empty_filter_is_accepted_as_noop() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter="),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(destination.exists());
}

/// A blank `--exclude=`/`--include=` value is also a no-op (exit 0).
#[test]
fn empty_exclude_and_include_are_accepted_as_noop() {
    for flag in ["--exclude=", "--include="] {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        std::fs::write(&source, b"data").expect("write source");
        let (code, _stdout, _stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(flag),
            source.into_os_string(),
            destination.clone().into_os_string(),
        ]);
        assert_eq!(code, 0, "{flag} should be accepted as a no-op");
        assert!(destination.exists(), "{flag} transfer should complete");
    }
}

/// A whitespace-only `--filter=" "` is NOT empty: a top-level `--filter` never
/// carries FILTRULE_WORD_SPLIT, so the space reaches the prefix switch and
/// upstream (exclude.c:1213) raises "Unknown filter rule" (RERR_SYNTAX = 1).
#[test]
fn whitespace_only_filter_is_still_rejected() {
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter= "),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    assert_ne!(code, 0);
}
