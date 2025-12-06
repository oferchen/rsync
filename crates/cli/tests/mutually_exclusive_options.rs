//! Tests for mutually exclusive option validation.
//!
//! Validates that conflicting options are properly rejected with
//! upstream-compatible error messages.

use cli::test_utils::parse_args;

// ============================================================================
// Delete Mode Mutual Exclusions
// ============================================================================

#[test]
fn test_delete_before_and_during_conflict() {
    let result = parse_args(["oc-rsync", "--delete-before", "--delete-during", "src", "dest"]);
    assert!(
        result.is_err(),
        "--delete-before and --delete-during should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_before_and_after_conflict() {
    let result = parse_args(["oc-rsync", "--delete-before", "--delete-after", "src", "dest"]);
    assert!(
        result.is_err(),
        "--delete-before and --delete-after should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_before_and_delay_conflict() {
    let result = parse_args(["oc-rsync", "--delete-before", "--delete-delay", "src", "dest"]);
    assert!(
        result.is_err(),
        "--delete-before and --delete-delay should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_during_and_after_conflict() {
    let result = parse_args(["oc-rsync", "--delete-during", "--delete-after", "src", "dest"]);
    assert!(
        result.is_err(),
        "--delete-during and --delete-after should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_during_and_delay_conflict() {
    let result = parse_args(["oc-rsync", "--delete-during", "--delete-delay", "src", "dest"]);
    assert!(
        result.is_err(),
        "--delete-during and --delete-delay should be mutually exclusive"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_after_and_delay_conflict() {
    let result = parse_args(["oc-rsync", "--delete-after", "--delete-delay", "src", "dest"]);
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
    assert!(
        result.is_err(),
        "Multiple delete modes should be rejected"
    );
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

// ============================================================================
// Multiple Value Rejections (usermap/groupmap)
// ============================================================================

#[test]
fn test_single_usermap_accepted() {
    let result = parse_args(["oc-rsync", "--usermap=foo:bar", "src", "dest"]);
    assert!(result.is_ok(), "Single --usermap should be accepted");
    let args = result.unwrap();
    assert_eq!(args.usermap, Some("foo:bar".into()));
}

#[test]
fn test_multiple_usermap_rejected() {
    let result = parse_args([
        "oc-rsync",
        "--usermap=foo:bar",
        "--usermap=baz:qux",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "Multiple --usermap options should be rejected"
    );
    let err = result.unwrap_err();
    // Clap reports this as ArgumentConflict when the same option is used multiple times
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_single_groupmap_accepted() {
    let result = parse_args(["oc-rsync", "--groupmap=admin:wheel", "src", "dest"]);
    assert!(result.is_ok(), "Single --groupmap should be accepted");
    let args = result.unwrap();
    assert_eq!(args.groupmap, Some("admin:wheel".into()));
}

#[test]
fn test_multiple_groupmap_rejected() {
    let result = parse_args([
        "oc-rsync",
        "--groupmap=admin:wheel",
        "--groupmap=users:staff",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "Multiple --groupmap options should be rejected"
    );
    let err = result.unwrap_err();
    // Clap reports this as ArgumentConflict when the same option is used multiple times
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
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

// ============================================================================
// Error Message Format Validation
// ============================================================================

#[test]
fn test_delete_conflict_error_message_contains_options() {
    let result = parse_args(["oc-rsync", "--delete-before", "--delete-after", "src", "dest"]);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    // Error message should mention the conflicting options
    assert!(
        err_msg.contains("delete") || err_msg.contains("exclusive"),
        "Error message should mention delete options or mutual exclusion"
    );
}

#[test]
fn test_usermap_too_many_error_message() {
    let result = parse_args([
        "oc-rsync",
        "--usermap=a:b",
        "--usermap=c:d",
        "src",
        "dest",
    ]);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    // Error message should mention usermap
    assert!(
        err_msg.contains("usermap") || err_msg.contains("once"),
        "Error message should mention usermap can only be specified once"
    );
}
