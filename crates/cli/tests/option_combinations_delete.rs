//! Tests for delete mode implicit activation via related options.
//!
//! Validates that certain options automatically enable delete mode when
//! no explicit delete mode is specified, matching upstream rsync behavior.

use cli::test_utils::parse_args;
use core::client::DeleteMode;

#[test]
fn test_delete_excluded_enables_delete_during() {
    let args = parse_args(["oc-rsync", "--delete-excluded", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::During,
        "--delete-excluded should implicitly enable delete mode (During)"
    );
    assert!(args.delete_excluded, "--delete-excluded flag should be set");
}

#[test]
fn test_max_delete_enables_delete_during() {
    let args = parse_args(["oc-rsync", "--max-delete=100", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::During,
        "--max-delete should implicitly enable delete mode (During)"
    );
    assert_eq!(
        args.max_delete,
        Some("100".into()),
        "--max-delete value should be captured"
    );
}

#[test]
fn test_explicit_delete_mode_takes_precedence_over_delete_excluded() {
    let args = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-excluded",
        "--dirs",
        "src",
        "dest",
    ])
    .unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::Before,
        "Explicit --delete-before should take precedence"
    );
    assert!(args.delete_excluded);
}

#[test]
fn test_explicit_delete_mode_takes_precedence_over_max_delete() {
    let args = parse_args([
        "oc-rsync",
        "--delete-after",
        "--max-delete=50",
        "--dirs",
        "src",
        "dest",
    ])
    .unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::After,
        "Explicit --delete-after should take precedence"
    );
    assert_eq!(args.max_delete, Some("50".into()));
}

#[test]
fn test_delete_excluded_without_delete_mode() {
    let args = parse_args(["oc-rsync", "--delete-excluded", "--dirs", "src", "dest"]).unwrap();
    assert!(
        args.delete_mode.is_enabled(),
        "--delete-excluded alone should enable delete mode"
    );
}

#[test]
fn test_max_delete_without_delete_mode() {
    let args = parse_args(["oc-rsync", "--max-delete=10", "--dirs", "src", "dest"]).unwrap();
    assert!(
        args.delete_mode.is_enabled(),
        "--max-delete alone should enable delete mode"
    );
}

#[test]
fn test_neither_delete_excluded_nor_max_delete() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::Disabled,
        "Delete mode should be disabled by default"
    );
    assert!(!args.delete_excluded);
    assert_eq!(args.max_delete, None);
}

#[test]
fn test_delete_excluded_and_max_delete_together() {
    let args = parse_args([
        "oc-rsync",
        "--delete-excluded",
        "--max-delete=20",
        "--dirs",
        "src",
        "dest",
    ])
    .unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::During,
        "Both options should enable delete mode"
    );
    assert!(args.delete_excluded);
    assert_eq!(args.max_delete, Some("20".into()));
}

#[test]
fn test_delete_flag_enables_during_mode() {
    let args = parse_args(["oc-rsync", "--delete", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::During,
        "--delete should enable During mode"
    );
}

#[test]
fn test_delete_during_explicit() {
    let args = parse_args(["oc-rsync", "--delete-during", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(args.delete_mode, DeleteMode::During);
}

#[test]
fn test_delete_before_explicit() {
    let args = parse_args(["oc-rsync", "--delete-before", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(args.delete_mode, DeleteMode::Before);
}

#[test]
fn test_delete_after_explicit() {
    let args = parse_args(["oc-rsync", "--delete-after", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(args.delete_mode, DeleteMode::After);
}

#[test]
fn test_delete_delay_explicit() {
    let args = parse_args(["oc-rsync", "--delete-delay", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(args.delete_mode, DeleteMode::Delay);
}
