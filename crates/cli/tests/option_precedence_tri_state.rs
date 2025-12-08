//! Tests for option precedence with tri-state flags.
//!
//! Validates last-wins behavior when positive and negative forms of
//! the same option are specified multiple times.

use cli::test_utils::parse_args;

// ============================================================================
// Owner/Group Preservation Precedence
// ============================================================================

#[test]
fn test_owner_last_wins_positive() {
    let args = parse_args(["oc-rsync", "--no-owner", "--owner", "src", "dest"]).unwrap();
    assert_eq!(
        args.owner,
        Some(true),
        "Last --owner should override --no-owner"
    );
}

#[test]
fn test_owner_last_wins_negative() {
    let args = parse_args(["oc-rsync", "--owner", "--no-owner", "src", "dest"]).unwrap();
    assert_eq!(
        args.owner,
        Some(false),
        "Last --no-owner should override --owner"
    );
}

#[test]
fn test_group_last_wins_positive() {
    let args = parse_args(["oc-rsync", "--no-group", "--group", "src", "dest"]).unwrap();
    assert_eq!(
        args.group,
        Some(true),
        "Last --group should override --no-group"
    );
}

#[test]
fn test_group_last_wins_negative() {
    let args = parse_args(["oc-rsync", "--group", "--no-group", "src", "dest"]).unwrap();
    assert_eq!(
        args.group,
        Some(false),
        "Last --no-group should override --group"
    );
}

// ============================================================================
// Permission/Time Preservation Precedence
// ============================================================================

#[test]
fn test_perms_last_wins_positive() {
    let args = parse_args(["oc-rsync", "--no-perms", "--perms", "src", "dest"]).unwrap();
    assert_eq!(
        args.perms,
        Some(true),
        "Last --perms should override --no-perms"
    );
}

#[test]
fn test_perms_last_wins_negative() {
    let args = parse_args(["oc-rsync", "--perms", "--no-perms", "src", "dest"]).unwrap();
    assert_eq!(
        args.perms,
        Some(false),
        "Last --no-perms should override --perms"
    );
}

#[test]
fn test_times_last_wins_positive() {
    let args = parse_args(["oc-rsync", "--no-times", "--times", "src", "dest"]).unwrap();
    assert_eq!(
        args.times,
        Some(true),
        "Last --times should override --no-times"
    );
}

#[test]
fn test_times_last_wins_negative() {
    let args = parse_args(["oc-rsync", "--times", "--no-times", "src", "dest"]).unwrap();
    assert_eq!(
        args.times,
        Some(false),
        "Last --no-times should override --times"
    );
}

// ============================================================================
// Device/Special File Preservation Precedence
// Note: --devices/--no-devices and --specials/--no-specials are configured
// as mutually exclusive in Clap, not as overridable tri-state flags
// ============================================================================

// ============================================================================
// Link Preservation Precedence
// Note: --hard-links/--no-hard-links and --links/--no-links are configured
// as mutually exclusive in Clap, not as overridable tri-state flags
// ============================================================================

// ============================================================================
// Extended Attributes Precedence
// ============================================================================

#[test]
fn test_acls_last_wins_positive() {
    let args = parse_args(["oc-rsync", "--no-acls", "--acls", "src", "dest"]).unwrap();
    assert_eq!(
        args.acls,
        Some(true),
        "Last --acls should override --no-acls"
    );
}

#[test]
fn test_acls_last_wins_negative() {
    let args = parse_args(["oc-rsync", "--acls", "--no-acls", "src", "dest"]).unwrap();
    assert_eq!(
        args.acls,
        Some(false),
        "Last --no-acls should override --acls"
    );
}

#[test]
fn test_xattrs_last_wins_positive() {
    let args = parse_args(["oc-rsync", "--no-xattrs", "--xattrs", "src", "dest"]).unwrap();
    assert_eq!(
        args.xattrs,
        Some(true),
        "Last --xattrs should override --no-xattrs"
    );
}

#[test]
fn test_xattrs_last_wins_negative() {
    let args = parse_args(["oc-rsync", "--xattrs", "--no-xattrs", "src", "dest"]).unwrap();
    assert_eq!(
        args.xattrs,
        Some(false),
        "Last --no-xattrs should override --xattrs"
    );
}

// ============================================================================
// Sparse/Checksum/Whole-file Precedence
// Note: --sparse/--no-sparse is configured as mutually exclusive in Clap,
// not as overridable tri-state flags
// ============================================================================

#[test]
fn test_checksum_last_wins_positive() {
    let args = parse_args(["oc-rsync", "--no-checksum", "--checksum", "src", "dest"]).unwrap();
    assert_eq!(
        args.checksum,
        Some(true),
        "Last --checksum should override --no-checksum"
    );
}

#[test]
fn test_checksum_last_wins_negative() {
    let args = parse_args(["oc-rsync", "--checksum", "--no-checksum", "src", "dest"]).unwrap();
    assert_eq!(
        args.checksum,
        Some(false),
        "Last --no-checksum should override --checksum"
    );
}

#[test]
fn test_whole_file_last_wins_positive() {
    let args = parse_args(["oc-rsync", "--no-whole-file", "--whole-file", "src", "dest"]).unwrap();
    assert_eq!(
        args.whole_file,
        Some(true),
        "Last --whole-file should override --no-whole-file"
    );
}

#[test]
fn test_whole_file_last_wins_negative() {
    let args = parse_args(["oc-rsync", "--whole-file", "--no-whole-file", "src", "dest"]).unwrap();
    assert_eq!(
        args.whole_file,
        Some(false),
        "Last --no-whole-file should override --whole-file"
    );
}

// ============================================================================
// Multiple Alternations
// ============================================================================

#[test]
fn test_multiple_owner_alternations() {
    let args = parse_args([
        "oc-rsync",
        "--owner",
        "--no-owner",
        "--owner",
        "--no-owner",
        "--owner",
        "src",
        "dest",
    ])
    .unwrap();
    assert_eq!(
        args.owner,
        Some(true),
        "Last --owner in sequence should win"
    );
}

#[test]
fn test_multiple_perms_alternations_ending_negative() {
    let args = parse_args([
        "oc-rsync",
        "--perms",
        "--no-perms",
        "--perms",
        "--no-perms",
        "src",
        "dest",
    ])
    .unwrap();
    assert_eq!(
        args.perms,
        Some(false),
        "Last --no-perms in sequence should win"
    );
}
