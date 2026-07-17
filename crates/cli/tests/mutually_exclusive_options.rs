//! Tests for mutually exclusive option validation.
//!
//! Validates that conflicting options are properly rejected with
//! upstream-compatible error messages.

use cli::test_utils::parse_args;

#[test]
fn test_delete_before_and_during_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-during",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "--delete-before and --delete-during should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_before_and_after_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-after",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "--delete-before and --delete-after should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_before_and_delay_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-delay",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "--delete-before and --delete-delay should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_during_and_after_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-during",
        "--delete-after",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "--delete-during and --delete-after should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_during_and_delay_are_same_when_term() {
    // upstream: options.c:724-725,2210 - `--delete-during` and `--delete-delay`
    // both write the single `delete_during` counter, so combining them selects
    // one "during" WHEN term and is NOT a conflict (`!!delete_during` == 1).
    let result = parse_args([
        "oc-rsync",
        "--dirs",
        "--delete-during",
        "--delete-delay",
        "src",
        "dest",
    ]);
    assert!(
        result.is_ok(),
        "--delete-during and --delete-delay share the 'during' WHEN term: {:?}",
        result.err()
    );
}

#[test]
fn test_delete_after_and_delay_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-after",
        "--delete-delay",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "--delete-after and --delete-delay should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_three_delete_modes_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-during",
        "--delete-after",
        "src",
        "dest",
    ]);
    assert!(result.is_err(), "Multiple delete modes should be rejected");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_all_delete_modes_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-during",
        "--delete-after",
        "--delete-delay",
        "src",
        "dest",
    ]);
    assert!(result.is_err(), "All delete modes together should fail");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_single_usermap_accepted() {
    let result = parse_args(["oc-rsync", "--usermap=foo:bar", "src", "dest"]);
    assert!(result.is_ok(), "Single --usermap should be accepted");
    let args = result.unwrap();
    assert_eq!(args.usermap, Some("foo:bar".into()));
}

#[test]
fn test_multiple_usermap_concatenated() {
    let result = parse_args([
        "oc-rsync",
        "--usermap=foo:bar",
        "--usermap=baz:qux",
        "src",
        "dest",
    ]);
    assert!(
        result.is_ok(),
        "Multiple --usermap options should be concatenated"
    );
    let args = result.unwrap();
    assert_eq!(args.usermap, Some("foo:bar,baz:qux".into()));
}

#[test]
fn test_single_groupmap_accepted() {
    let result = parse_args(["oc-rsync", "--groupmap=admin:wheel", "src", "dest"]);
    assert!(result.is_ok(), "Single --groupmap should be accepted");
    let args = result.unwrap();
    assert_eq!(args.groupmap, Some("admin:wheel".into()));
}

#[test]
fn test_multiple_groupmap_concatenated() {
    let result = parse_args([
        "oc-rsync",
        "--groupmap=admin:wheel",
        "--groupmap=users:staff",
        "src",
        "dest",
    ]);
    assert!(
        result.is_ok(),
        "Multiple --groupmap options should be concatenated"
    );
    let args = result.unwrap();
    assert_eq!(args.groupmap, Some("admin:wheel,users:staff".into()));
}

#[test]
fn test_usermap_and_groupmap_together() {
    let result = parse_args([
        "oc-rsync",
        "--usermap=foo:bar",
        "--groupmap=admin:wheel",
        "src",
        "dest",
    ]);
    assert!(
        result.is_ok(),
        "One usermap and one groupmap together should be accepted"
    );
    let args = result.unwrap();
    assert_eq!(args.usermap, Some("foo:bar".into()));
    assert_eq!(args.groupmap, Some("admin:wheel".into()));
}

#[test]
fn test_delete_conflict_error_message_contains_options() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-after",
        "src",
        "dest",
    ]);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    // Error message should mention the conflicting options
    assert!(
        err_msg.contains("delete") || err_msg.contains("exclusive"),
        "Error message should mention delete options or mutual exclusion"
    );
}

#[test]
fn test_multiple_usermap_concatenation_preserves_order() {
    let result = parse_args(["oc-rsync", "--usermap=a:b", "--usermap=c:d", "src", "dest"]);
    let args = result.expect("multiple --usermap should succeed");
    assert_eq!(args.usermap, Some("a:b,c:d".into()));
}

#[test]
fn test_append_and_whole_file_rejected() {
    // upstream: options.c:2382 - --append cannot be used with --whole-file.
    // Both flags parse cleanly individually; the conflict surfaces at config
    // build time and is reported through the rsync syntax-error path
    // (exit code 1).
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run(
        ["oc-rsync", "--append", "--whole-file", "src", "dest"],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(code, 1, "expected RERR_SYNTAX (exit code 1)");
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert!(
        stderr_text.contains("--append") && stderr_text.contains("--whole-file"),
        "stderr should name both conflicting flags, got: {stderr_text}"
    );
    assert!(
        stderr_text.contains("cannot be used with"),
        "stderr should use upstream phrasing, got: {stderr_text}"
    );
}

#[test]
fn test_old_args_and_secluded_args_rejected() {
    // upstream: options.c:1977 - `--old-args` and `--secluded-args` are mutually
    // exclusive and abort with exit 1. Unlike most conflicts, upstream phrases
    // this one as "--secluded-args conflicts with --old-args." (secluded-args
    // named first, trailing period), which oc must reproduce verbatim.
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run(
        ["oc-rsync", "--old-args", "--secluded-args", "src", "dest"],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(code, 1, "expected RERR_SYNTAX (exit code 1)");
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert!(
        stderr_text.contains("--secluded-args conflicts with --old-args."),
        "stderr must use upstream's exact phrasing, got: {stderr_text}"
    );
}

#[test]
fn test_append_and_no_whole_file_accepted() {
    // The companion `--no-whole-file` form must remain accepted.
    let result = parse_args(["oc-rsync", "--append", "--no-whole-file", "src", "dest"]);
    assert!(
        result.is_ok(),
        "--append --no-whole-file should parse without conflict"
    );
}
