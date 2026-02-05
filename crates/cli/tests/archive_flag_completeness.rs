//! Comprehensive tests for --archive (-a) flag completeness.
//!
//! The archive flag (-a) is a shorthand that enables the following sub-options:
//! -r (recursive), -l (symlinks), -p (permissions), -t (times),
//! -g (group), -o (owner), -D (devices + specials)
//!
//! These tests verify that parsing --archive correctly enables all expected
//! sub-options and that the flag behaves correctly in combination with other
//! flags and explicit overrides.

use cli::test_utils::parse_args;

// ============================================================================
// Part 1: Verify --archive Enables All Expected Sub-Options at Parse Time
// ============================================================================

#[test]
fn archive_flag_sets_archive_boolean() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert!(args.archive, "-a should set the archive flag to true");
}

#[test]
fn archive_flag_enables_recursive() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert!(
        args.recursive,
        "-a should enable recursive mode (-r) at parse time"
    );
}

/// Note: The archive flag at parse time sets archive=true but does NOT
/// explicitly set the optional flags (links, perms, times, group, owner,
/// devices, specials) to Some(true). These are resolved during execution.
/// This test documents the current parse-time behavior.
#[test]
fn archive_flag_parse_time_optional_flags_are_none() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();

    // At parse time, archive is set but optional sub-flags remain None.
    // The execution logic expands archive=true to enable these flags.
    assert!(args.archive);

    // These are None at parse time; execution will treat them as enabled
    // because archive=true. Document this for clarity:
    assert_eq!(
        args.links, None,
        "links remains None at parse time (archive expansion happens in execution)"
    );
    assert_eq!(
        args.perms, None,
        "perms remains None at parse time (archive expansion happens in execution)"
    );
    assert_eq!(
        args.times, None,
        "times remains None at parse time (archive expansion happens in execution)"
    );
    assert_eq!(
        args.group, None,
        "group remains None at parse time (archive expansion happens in execution)"
    );
    assert_eq!(
        args.owner, None,
        "owner remains None at parse time (archive expansion happens in execution)"
    );
    assert_eq!(
        args.devices, None,
        "devices remains None at parse time (archive expansion happens in execution)"
    );
    assert_eq!(
        args.specials, None,
        "specials remains None at parse time (archive expansion happens in execution)"
    );
}

#[test]
fn archive_long_form_equivalent_to_short() {
    let short = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    let long = parse_args(["oc-rsync", "--archive", "src", "dest"]).unwrap();

    assert_eq!(short.archive, long.archive);
    assert_eq!(short.recursive, long.recursive);
    assert_eq!(short.links, long.links);
    assert_eq!(short.perms, long.perms);
    assert_eq!(short.times, long.times);
    assert_eq!(short.group, long.group);
    assert_eq!(short.owner, long.owner);
    assert_eq!(short.devices, long.devices);
    assert_eq!(short.specials, long.specials);
}

// ============================================================================
// Part 2: Verify -rlptgoD Individually Set Matches Archive Semantics
// ============================================================================

#[test]
fn explicit_rlptgod_flags_set_explicit_values() {
    // When -rlptgoD is specified explicitly (rather than -a), the flags are
    // explicitly set to Some(true).
    let args = parse_args(["oc-rsync", "-rlptgoD", "src", "dest"]).unwrap();

    assert!(args.recursive, "-r should set recursive to true");
    assert_eq!(args.links, Some(true), "-l should set links to Some(true)");
    assert_eq!(args.perms, Some(true), "-p should set perms to Some(true)");
    assert_eq!(args.times, Some(true), "-t should set times to Some(true)");
    assert_eq!(args.group, Some(true), "-g should set group to Some(true)");
    assert_eq!(args.owner, Some(true), "-o should set owner to Some(true)");
    assert_eq!(
        args.devices,
        Some(true),
        "-D should set devices to Some(true)"
    );
    assert_eq!(
        args.specials,
        Some(true),
        "-D should set specials to Some(true)"
    );
}

#[test]
fn archive_without_explicit_flags_leaves_flags_none() {
    // This demonstrates the difference between -a and -rlptgoD at parse time
    let archive_args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    let explicit_args = parse_args(["oc-rsync", "-rlptgoD", "src", "dest"]).unwrap();

    // Both should enable recursive
    assert!(archive_args.recursive);
    assert!(explicit_args.recursive);

    // But -a leaves optional flags as None (expansion in execution)
    assert_eq!(archive_args.links, None);
    assert_eq!(explicit_args.links, Some(true));
}

// ============================================================================
// Part 3: Override Behavior - Explicit Flags After Archive
// ============================================================================

#[test]
fn no_recursive_after_archive_disables_recursion() {
    let args = parse_args(["oc-rsync", "-a", "--no-recursive", "src", "dest"]).unwrap();
    assert!(args.archive, "archive flag should still be set");
    assert!(!args.recursive, "--no-recursive should disable recursion");
    assert_eq!(
        args.recursive_override,
        Some(false),
        "recursive_override should be explicitly false"
    );
}

#[test]
fn no_links_after_archive_disables_symlinks() {
    let args = parse_args(["oc-rsync", "-a", "--no-links", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.links,
        Some(false),
        "--no-links should disable symlink preservation"
    );
}

#[test]
fn no_perms_after_archive_disables_permissions() {
    let args = parse_args(["oc-rsync", "-a", "--no-perms", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.perms,
        Some(false),
        "--no-perms should disable permission preservation"
    );
}

#[test]
fn no_times_after_archive_disables_times() {
    let args = parse_args(["oc-rsync", "-a", "--no-times", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.times,
        Some(false),
        "--no-times should disable time preservation"
    );
}

#[test]
fn no_group_after_archive_disables_group() {
    let args = parse_args(["oc-rsync", "-a", "--no-group", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.group,
        Some(false),
        "--no-group should disable group preservation"
    );
}

#[test]
fn no_owner_after_archive_disables_owner() {
    let args = parse_args(["oc-rsync", "-a", "--no-owner", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.owner,
        Some(false),
        "--no-owner should disable owner preservation"
    );
}

#[test]
fn no_devices_after_archive_disables_devices() {
    let args = parse_args(["oc-rsync", "-a", "--no-devices", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.devices,
        Some(false),
        "--no-devices should disable device preservation"
    );
}

#[test]
fn no_specials_after_archive_disables_specials() {
    let args = parse_args(["oc-rsync", "-a", "--no-specials", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.specials,
        Some(false),
        "--no-specials should disable special file preservation"
    );
}

#[test]
fn no_d_after_archive_disables_devices_and_specials() {
    let args = parse_args(["oc-rsync", "-a", "--no-D", "src", "dest"]).unwrap();
    assert!(args.archive);
    // --no-D should disable both devices and specials
    assert_eq!(
        args.devices,
        Some(false),
        "--no-D should disable device preservation"
    );
    assert_eq!(
        args.specials,
        Some(false),
        "--no-D should disable special file preservation"
    );
}

// ============================================================================
// Part 4: Override Behavior - Explicit Flags Before Archive
// ============================================================================

#[test]
fn no_perms_before_archive_stays_disabled() {
    let args = parse_args(["oc-rsync", "--no-perms", "-a", "src", "dest"]).unwrap();
    assert!(args.archive);
    // Even with -a, --no-perms specified before should remain
    assert_eq!(
        args.perms,
        Some(false),
        "--no-perms before -a should stay disabled"
    );
}

#[test]
fn no_times_before_archive_stays_disabled() {
    let args = parse_args(["oc-rsync", "--no-times", "-a", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.times,
        Some(false),
        "--no-times before -a should stay disabled"
    );
}

#[test]
fn no_owner_before_archive_stays_disabled() {
    let args = parse_args(["oc-rsync", "--no-owner", "-a", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.owner,
        Some(false),
        "--no-owner before -a should stay disabled"
    );
}

#[test]
fn no_group_before_archive_stays_disabled() {
    let args = parse_args(["oc-rsync", "--no-group", "-a", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.group,
        Some(false),
        "--no-group before -a should stay disabled"
    );
}

// ============================================================================
// Part 5: Archive Does NOT Enable Non-Archive Flags
// ============================================================================

#[test]
fn archive_does_not_enable_acls() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert_ne!(
        args.acls,
        Some(true),
        "-a should NOT enable ACLs (requires -A)"
    );
}

#[test]
fn archive_does_not_enable_xattrs() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert_ne!(
        args.xattrs,
        Some(true),
        "-a should NOT enable extended attributes (requires -X)"
    );
}

#[test]
fn archive_does_not_enable_hard_links() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert_ne!(
        args.hard_links,
        Some(true),
        "-a should NOT enable hard links (requires -H)"
    );
}

#[test]
fn archive_does_not_enable_compression() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert!(
        !args.compress,
        "-a should NOT enable compression (requires -z)"
    );
}

#[test]
fn archive_does_not_enable_sparse() {
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert_ne!(
        args.sparse,
        Some(true),
        "-a should NOT enable sparse handling (requires -S)"
    );
}

#[test]
fn archive_does_not_enable_delete() {
    use core::client::DeleteMode;
    let args = parse_args(["oc-rsync", "-a", "src", "dest"]).unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::Disabled,
        "-a should NOT enable deletion"
    );
}

// ============================================================================
// Part 6: Archive Combined with Additional Flags
// ============================================================================

#[test]
fn archive_with_acls() {
    let args = parse_args(["oc-rsync", "-a", "-A", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(args.acls, Some(true), "-A should enable ACLs with -a");
}

#[test]
fn archive_with_xattrs() {
    let args = parse_args(["oc-rsync", "-a", "-X", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(args.xattrs, Some(true), "-X should enable xattrs with -a");
}

#[test]
fn archive_with_hard_links() {
    let args = parse_args(["oc-rsync", "-a", "-H", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.hard_links,
        Some(true),
        "-H should enable hard links with -a"
    );
}

#[test]
fn archive_with_compress() {
    let args = parse_args(["oc-rsync", "-a", "-z", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.compress, "-z should enable compression with -a");
}

#[test]
fn archive_with_verbose() {
    let args = parse_args(["oc-rsync", "-a", "-v", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.verbosity > 0, "-v should increase verbosity with -a");
}

#[test]
fn archive_with_progress() {
    use cli::test_utils::ProgressSetting;
    let args = parse_args(["oc-rsync", "-a", "--progress", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.progress,
        ProgressSetting::PerFile,
        "--progress should work with -a"
    );
}

// ============================================================================
// Part 7: Combined Short Option Parsing
// ============================================================================

#[test]
fn combined_av_flags() {
    let args = parse_args(["oc-rsync", "-av", "src", "dest"]).unwrap();
    assert!(args.archive, "-av should enable archive");
    assert!(args.verbosity > 0, "-av should enable verbose");
}

#[test]
fn combined_az_flags() {
    let args = parse_args(["oc-rsync", "-az", "src", "dest"]).unwrap();
    assert!(args.archive, "-az should enable archive");
    assert!(args.compress, "-az should enable compression");
}

#[test]
fn combined_avz_flags() {
    let args = parse_args(["oc-rsync", "-avz", "src", "dest"]).unwrap();
    assert!(args.archive, "-avz should enable archive");
    assert!(args.verbosity > 0, "-avz should enable verbose");
    assert!(args.compress, "-avz should enable compression");
}

#[test]
fn combined_avzh_flags() {
    use core::client::HumanReadableMode;
    let args = parse_args(["oc-rsync", "-avzh", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.verbosity > 0);
    assert!(args.compress);
    assert_eq!(
        args.human_readable,
        Some(HumanReadableMode::Enabled),
        "-h should enable human-readable output"
    );
}

#[test]
fn combined_avzp_flags_with_progress() {
    use cli::test_utils::ProgressSetting;
    let args = parse_args(["oc-rsync", "-avz", "--progress", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.verbosity > 0);
    assert!(args.compress);
    assert_eq!(args.progress, ProgressSetting::PerFile);
}

// ============================================================================
// Part 8: Multiple Override Scenarios
// ============================================================================

#[test]
fn archive_with_multiple_disables() {
    let args = parse_args([
        "oc-rsync",
        "-a",
        "--no-perms",
        "--no-times",
        "--no-owner",
        "src",
        "dest",
    ])
    .unwrap();

    assert!(args.archive);
    assert!(args.recursive); // Not overridden
    assert_eq!(args.perms, Some(false));
    assert_eq!(args.times, Some(false));
    assert_eq!(args.owner, Some(false));
    assert_eq!(args.group, None); // Not overridden
    assert_eq!(args.links, None); // Not overridden
}

#[test]
fn archive_with_conflicting_flags_last_wins() {
    // Test that the last specified flag wins
    let args = parse_args(["oc-rsync", "-a", "--no-perms", "--perms", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.perms,
        Some(true),
        "--perms after --no-perms should enable perms"
    );
}

#[test]
fn archive_with_explicit_enable_after_disable() {
    let args = parse_args(["oc-rsync", "-a", "--no-times", "--times", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.times,
        Some(true),
        "--times after --no-times should enable times"
    );
}

// ============================================================================
// Part 9: Default State Without Archive
// ============================================================================

#[test]
fn no_archive_by_default() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.archive, "Archive should be disabled by default");
    assert!(!args.recursive, "Recursive should be disabled by default");
    assert_eq!(args.links, None, "Links should be None by default");
    assert_eq!(args.perms, None, "Perms should be None by default");
    assert_eq!(args.times, None, "Times should be None by default");
    assert_eq!(args.group, None, "Group should be None by default");
    assert_eq!(args.owner, None, "Owner should be None by default");
    assert_eq!(args.devices, None, "Devices should be None by default");
    assert_eq!(args.specials, None, "Specials should be None by default");
}

// ============================================================================
// Part 10: Device and Special File Flag (-D) Behavior
// ============================================================================

#[test]
fn d_flag_enables_both_devices_and_specials() {
    let args = parse_args(["oc-rsync", "-D", "src", "dest"]).unwrap();
    assert_eq!(
        args.devices,
        Some(true),
        "-D should enable devices preservation"
    );
    assert_eq!(
        args.specials,
        Some(true),
        "-D should enable specials preservation"
    );
}

#[test]
fn devices_flag_only_enables_devices() {
    let args = parse_args(["oc-rsync", "--devices", "src", "dest"]).unwrap();
    assert_eq!(
        args.devices,
        Some(true),
        "--devices should enable devices preservation"
    );
    assert_eq!(
        args.specials, None,
        "--devices should NOT enable specials preservation"
    );
}

#[test]
fn specials_flag_only_enables_specials() {
    let args = parse_args(["oc-rsync", "--specials", "src", "dest"]).unwrap();
    assert_eq!(
        args.specials,
        Some(true),
        "--specials should enable specials preservation"
    );
    assert_eq!(
        args.devices, None,
        "--specials should NOT enable devices preservation"
    );
}

#[test]
fn archive_with_no_d_disables_both() {
    let args = parse_args(["oc-rsync", "-a", "--no-D", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.devices,
        Some(false),
        "-a --no-D should disable devices"
    );
    assert_eq!(
        args.specials,
        Some(false),
        "-a --no-D should disable specials"
    );
}

// ============================================================================
// Part 11: Symlink Flag (-l/--links) Behavior
// ============================================================================

#[test]
fn links_flag_short() {
    let args = parse_args(["oc-rsync", "-l", "src", "dest"]).unwrap();
    assert_eq!(
        args.links,
        Some(true),
        "-l should enable symlink preservation"
    );
}

#[test]
fn links_flag_long() {
    let args = parse_args(["oc-rsync", "--links", "src", "dest"]).unwrap();
    assert_eq!(
        args.links,
        Some(true),
        "--links should enable symlink preservation"
    );
}

#[test]
fn no_links_flag() {
    let args = parse_args(["oc-rsync", "--no-links", "src", "dest"]).unwrap();
    assert_eq!(
        args.links,
        Some(false),
        "--no-links should disable symlink preservation"
    );
}

#[test]
fn archive_symlinks_override() {
    let args = parse_args(["oc-rsync", "-a", "--no-links", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.links,
        Some(false),
        "--no-links should override archive's implicit symlink handling"
    );
}

// ============================================================================
// Part 12: Ensure Correct Expansion in Combined Flags
// ============================================================================

#[test]
fn combined_rlptgod_equals_full_expansion() {
    let args = parse_args(["oc-rsync", "-rlptgoD", "src", "dest"]).unwrap();

    // Verify each component is explicitly set
    assert!(args.recursive, "-r component should be enabled");
    assert_eq!(args.links, Some(true), "-l component should be enabled");
    assert_eq!(args.perms, Some(true), "-p component should be enabled");
    assert_eq!(args.times, Some(true), "-t component should be enabled");
    assert_eq!(args.group, Some(true), "-g component should be enabled");
    assert_eq!(args.owner, Some(true), "-o component should be enabled");
    assert_eq!(
        args.devices,
        Some(true),
        "-D component should enable devices"
    );
    assert_eq!(
        args.specials,
        Some(true),
        "-D component should enable specials"
    );
}

// ============================================================================
// Part 13: Edge Cases and Special Scenarios
// ============================================================================

/// Note: Multiple -a flags conflict in clap's argument parser, so this test
/// verifies the expected behavior of rejecting repeated archive flags.
#[test]
fn archive_repeated_causes_error() {
    let result = parse_args(["oc-rsync", "-a", "-a", "-a", "src", "dest"]);
    assert!(
        result.is_err(),
        "Multiple -a flags should cause an argument conflict error"
    );
}

#[test]
fn archive_with_dry_run() {
    let args = parse_args(["oc-rsync", "-an", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert!(args.dry_run, "-n should enable dry-run with -a");
}

#[test]
fn archive_with_checksum() {
    let args = parse_args(["oc-rsync", "-ac", "src", "dest"]).unwrap();
    assert!(args.archive);
    assert_eq!(
        args.checksum,
        Some(true),
        "-c should enable checksum with -a"
    );
}

#[test]
fn archive_preserves_correct_order_of_operations() {
    // Verify that the order of flags produces correct results
    let args1 = parse_args(["oc-rsync", "-a", "--no-perms", "src", "dest"]).unwrap();
    let args2 = parse_args(["oc-rsync", "--no-perms", "-a", "src", "dest"]).unwrap();

    // Both should have archive=true
    assert!(args1.archive);
    assert!(args2.archive);

    // Both should have perms=Some(false) because explicit --no-perms was specified
    assert_eq!(args1.perms, Some(false));
    assert_eq!(args2.perms, Some(false));
}
