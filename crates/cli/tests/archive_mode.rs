//! Tests for archive mode (-a) flag behavior and override semantics.
//!
//! Note: The -a flag sets archive=true at parse time, but the expansion to
//! -rlptgoD happens later in the execution logic. These tests validate the
//! archive flag itself and the interaction with explicit override flags.

use cli::test_utils::parse_args;

// ============================================================================
// Archive Mode Flag
// ============================================================================

#[test]
fn test_archive_flag_sets_archive_mode() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert!(args.archive, "-a should set archive flag");
}

#[test]
fn test_archive_long_form() {
    let args = parse_args(["oc-rsync", "--archive", "src", "dest"]).unwrap();
    assert!(args.archive, "--archive should set archive flag");
}

#[test]
fn test_no_archive_by_default() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.archive, "Archive should be disabled by default");
}

// ============================================================================
// Archive Mode Does NOT Enable Extra Flags at Parse Time
// ============================================================================

#[test]
fn test_archive_does_not_enable_acls() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert_ne!(args.acls, Some(true), "-a should NOT imply -A (ACLs)");
}

#[test]
fn test_archive_does_not_enable_xattrs() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert_ne!(
        args.xattrs,
        Some(true),
        "-a should NOT imply -X (extended attributes)"
    );
}

#[test]
fn test_archive_does_not_enable_hard_links() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert_ne!(
        args.hard_links,
        Some(true),
        "-a should NOT imply -H (preserve hard links)"
    );
}

// ============================================================================
// Override Behavior: Explicit Flags After Archive Override Defaults
// ============================================================================

#[test]
fn test_no_perms_overrides_archive() {
    let args = parse_args(["oc-rsync", "-a", "--no-perms", "src", "dest"]).unwrap();
    assert_eq!(
        args.perms,
        Some(false),
        "--no-perms after -a should disable perms"
    );
    assert!(args.archive, "Archive flag should still be set");
}

#[test]
fn test_no_times_overrides_archive() {
    let args = parse_args(["oc-rsync", "-a", "--no-times", "src", "dest"]).unwrap();
    assert_eq!(
        args.times,
        Some(false),
        "--no-times after -a should disable times"
    );
    assert!(args.archive);
}

#[test]
fn test_no_owner_overrides_archive() {
    let args = parse_args(["oc-rsync", "-a", "--no-owner", "src", "dest"]).unwrap();
    assert_eq!(
        args.owner,
        Some(false),
        "--no-owner after -a should disable owner"
    );
    assert!(args.archive);
}

#[test]
fn test_no_group_overrides_archive() {
    let args = parse_args(["oc-rsync", "-a", "--no-group", "src", "dest"]).unwrap();
    assert_eq!(
        args.group,
        Some(false),
        "--no-group after -a should disable group"
    );
    assert!(args.archive);
}

#[test]
fn test_no_recursive_overrides_archive() {
    let args = parse_args(["oc-rsync", "-a", "--no-recursive", "src", "dest"]).unwrap();
    assert_eq!(
        args.recursive_override,
        Some(false),
        "--no-recursive after -a should disable recursion"
    );
    assert!(args.archive);
}

// ============================================================================
// Explicit Flags Before Archive: Archive Doesn't Override
// ============================================================================

#[test]
fn test_no_perms_before_archive_stays_disabled() {
    let args = parse_args(["oc-rsync", "--no-perms", "-a", "src", "dest"]).unwrap();
    // -a doesn't re-enable perms if --no-perms was already set
    // The archive flag is set, but perms stays explicitly disabled
    assert!(args.archive, "-a should set archive flag");
    assert_eq!(
        args.perms,
        Some(false),
        "--no-perms should remain disabled even with -a"
    );
}

#[test]
fn test_no_owner_before_archive_stays_disabled() {
    let args = parse_args(["oc-rsync", "--no-owner", "-a", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.owner,
        Some(false),
        "--no-owner should remain disabled even with -a"
    );
}

#[test]
fn test_no_group_before_archive_stays_disabled() {
    let args = parse_args(["oc-rsync", "--no-group", "-a", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.group,
        Some(false),
        "--no-group should remain disabled even with -a"
    );
}

// ============================================================================
// Archive with Additional Flags
// ============================================================================

#[test]
fn test_archive_with_acls() {
    let args = parse_args(["oc-rsync", "-a", "-A", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(args.acls, Some(true), "-A should enable ACLs");
    // Note: -A implying perms happens in execution logic, not at parse time
}

#[test]
fn test_archive_with_xattrs() {
    let args = parse_args(["oc-rsync", "-a", "-X", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(args.xattrs, Some(true), "-X should enable xattrs");
}

#[test]
fn test_archive_with_hard_links() {
    let args = parse_args(["oc-rsync", "-a", "-H", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(args.hard_links, Some(true), "-H should enable hard links");
}

#[test]
fn test_archive_with_compress() {
    let args = parse_args(["oc-rsync", "-a", "-z", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.compress, "-z should enable compression");
}

#[test]
fn test_archive_with_verbose() {
    let args = parse_args(["oc-rsync", "-a", "-v", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.verbosity > 0, "-v should increase verbosity");
}

// ============================================================================
// Archive with Multiple Overrides
// ============================================================================

#[test]
fn test_archive_with_multiple_overrides() {
    let args = parse_args(["oc-rsync", "-a", "--no-perms", "--no-owner", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(args.perms, Some(false));
    assert_eq!(args.owner, Some(false));
}

#[test]
fn test_archive_with_mixed_overrides() {
    let args = parse_args(["oc-rsync", "-a", "--no-perms", "-A", "src", "dest"]).unwrap();
    assert!(args.archive);
    // Note: --no-perms stays disabled at parse time; -A will imply perms in execution logic
    assert_eq!(
        args.perms,
        Some(false),
        "--no-perms should stay disabled at parse time"
    );
    assert_eq!(args.acls, Some(true), "-A should enable ACLs");
}

// ============================================================================
// Archive Combined with Short Options
// ============================================================================

#[test]
fn test_archive_combined_with_v() {
    let args = parse_args(["oc-rsync", "-av", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.verbosity > 0);
}

#[test]
fn test_archive_combined_with_z() {
    let args = parse_args(["oc-rsync", "-az", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.compress);
}

#[test]
fn test_archive_combined_with_multiple() {
    let args = parse_args(["oc-rsync", "-avz", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.verbosity > 0);
    assert!(args.compress);
}
