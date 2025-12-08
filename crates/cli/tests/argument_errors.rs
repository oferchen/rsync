//! Tests for argument parsing error handling and error messages.
//!
//! Validates that invalid argument combinations produce appropriate
//! error messages at parse time.
//!
//! Note: Many value validations (like invalid numbers, sizes, etc.) happen
//! during execution, not at parse time, so they are not tested here.

use clap::error::ErrorKind;
use cli::test_utils::parse_args;

// ============================================================================
// Conflicting Delete Modes
// ============================================================================

#[test]
fn test_delete_before_and_after_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-after",
        "src",
        "dest",
    ]);
    assert!(result.is_err(), "Conflicting delete modes should fail");
    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::ArgumentConflict,
        "Should produce ArgumentConflict error"
    );
}

#[test]
fn test_delete_before_and_during_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-during",
        "src",
        "dest",
    ]);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
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
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
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
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn test_delete_during_and_delay_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--delete-during",
        "--delete-delay",
        "src",
        "dest",
    ]);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
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
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
}

// ============================================================================
// Duplicate Options (ArgumentConflict)
// ============================================================================

#[test]
fn test_multiple_usermap_conflict() {
    let result = parse_args(["oc-rsync", "--usermap=a:b", "--usermap=c:d", "src", "dest"]);
    assert!(result.is_err(), "Multiple usermap should fail");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn test_multiple_groupmap_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--groupmap=a:b",
        "--groupmap=c:d",
        "src",
        "dest",
    ]);
    assert!(result.is_err(), "Multiple groupmap should fail");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn test_temp_dir_and_tmp_dir_conflict() {
    let result = parse_args([
        "oc-rsync",
        "--temp-dir=/tmp1",
        "--tmp-dir=/tmp2",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "--temp-dir and --tmp-dir are aliases and should conflict"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
}

// ============================================================================
// Error Message Content Validation
// ============================================================================

#[test]
fn test_delete_conflict_error_message() {
    let result = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-after",
        "src",
        "dest",
    ]);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    // Error should mention delete or conflict
    assert!(
        err_msg.to_lowercase().contains("delete")
            || err_msg.to_lowercase().contains("conflict")
            || err_msg.contains("--delete-before")
            || err_msg.contains("--delete-after"),
        "Error message should mention conflicting delete options: {err_msg}"
    );
}

#[test]
fn test_usermap_conflict_error_message() {
    let result = parse_args(["oc-rsync", "--usermap=a:b", "--usermap=c:d", "src", "dest"]);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.to_lowercase().contains("usermap") || err_msg.to_lowercase().contains("conflict"),
        "Error message should mention usermap or conflict: {err_msg}"
    );
}

#[test]
fn test_groupmap_conflict_error_message() {
    let result = parse_args([
        "oc-rsync",
        "--groupmap=a:b",
        "--groupmap=c:d",
        "src",
        "dest",
    ]);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.to_lowercase().contains("groupmap") || err_msg.to_lowercase().contains("conflict"),
        "Error message should mention groupmap or conflict: {err_msg}"
    );
}

// ============================================================================
// Valid Cases That Should NOT Error
// ============================================================================

#[test]
fn test_single_delete_mode_valid() {
    let result = parse_args(["oc-rsync", "--delete-before", "src", "dest"]);
    assert!(result.is_ok(), "Single delete mode should be valid");
}

#[test]
fn test_single_usermap_valid() {
    let result = parse_args(["oc-rsync", "--usermap=foo:bar", "src", "dest"]);
    assert!(result.is_ok(), "Single usermap should be valid");
}

#[test]
fn test_single_groupmap_valid() {
    let result = parse_args(["oc-rsync", "--groupmap=admin:wheel", "src", "dest"]);
    assert!(result.is_ok(), "Single groupmap should be valid");
}

#[test]
fn test_usermap_and_groupmap_together_valid() {
    let result = parse_args([
        "oc-rsync",
        "--usermap=foo:bar",
        "--groupmap=admin:wheel",
        "src",
        "dest",
    ]);
    assert!(
        result.is_ok(),
        "One usermap and one groupmap should be valid together"
    );
}

#[test]
fn test_basic_invocation_valid() {
    let result = parse_args(["oc-rsync", "src", "dest"]);
    assert!(result.is_ok(), "Basic invocation should be valid");
}

#[test]
fn test_archive_with_source_dest_valid() {
    let result = parse_args(["oc-rsync", "-a", "src", "dest"]);
    assert!(result.is_ok(), "Archive with source/dest should be valid");
}

#[test]
fn test_verbose_compress_valid() {
    let result = parse_args(["oc-rsync", "-vz", "src", "dest"]);
    assert!(result.is_ok(), "Combined short options should be valid");
}
