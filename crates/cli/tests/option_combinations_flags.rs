//! Tests for option combinations that enable or override related flags.
//!
//! Validates implicit flag activation and override behavior matching
//! upstream rsync semantics.

use cli::test_utils::parse_args;

// ============================================================================
// Backup Mode Activation
// ============================================================================

#[test]
fn test_backup_flag_alone() {
    let args = parse_args(["oc-rsync", "--backup", "src", "dest"]).unwrap();
    assert!(args.backup, "--backup flag should enable backup mode");
    assert_eq!(args.backup_dir, None);
    assert_eq!(args.backup_suffix, None);
}

#[test]
fn test_backup_dir_implies_backup() {
    let args = parse_args(["oc-rsync", "--backup-dir=/backups", "src", "dest"]).unwrap();
    assert!(
        args.backup,
        "--backup-dir should implicitly enable backup mode"
    );
    assert_eq!(args.backup_dir, Some("/backups".into()));
}

#[test]
fn test_backup_suffix_implies_backup() {
    let args = parse_args(["oc-rsync", "--suffix=.bak", "src", "dest"]).unwrap();
    assert!(args.backup, "--suffix should implicitly enable backup mode");
    assert_eq!(args.backup_suffix, Some(".bak".into()));
}

#[test]
fn test_backup_dir_and_suffix_together() {
    let args = parse_args(["oc-rsync", "--backup-dir=/tmp", "--suffix=~", "src", "dest"]).unwrap();
    assert!(args.backup);
    assert_eq!(args.backup_dir, Some("/tmp".into()));
    assert_eq!(args.backup_suffix, Some("~".into()));
}

#[test]
fn test_no_backup_options() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.backup, "Backup should be disabled by default");
    assert_eq!(args.backup_dir, None);
    assert_eq!(args.backup_suffix, None);
}

// ============================================================================
// Compression Activation and Override
// ============================================================================

#[test]
fn test_compress_flag_alone() {
    let args = parse_args(["oc-rsync", "--compress", "src", "dest"]).unwrap();
    assert!(args.compress, "--compress should enable compression");
    assert!(!args.no_compress);
}

#[test]
fn test_no_compress_flag_alone() {
    let args = parse_args(["oc-rsync", "--no-compress", "src", "dest"]).unwrap();
    assert!(!args.compress, "--no-compress should disable compression");
    assert!(args.no_compress);
}

#[test]
fn test_compress_level_overrides_no_compress() {
    let args = parse_args([
        "oc-rsync",
        "--no-compress",
        "--compress-level=6",
        "src",
        "dest",
    ])
    .unwrap();
    assert!(
        args.compress,
        "--compress-level should override --no-compress"
    );
    assert_eq!(args.compress_level, Some("6".into()));
}

#[test]
fn test_compress_level_enables_compression() {
    let args = parse_args(["oc-rsync", "--compress-level=9", "src", "dest"]).unwrap();
    assert!(
        args.compress,
        "--compress-level should implicitly enable compression"
    );
    assert_eq!(args.compress_level, Some("9".into()));
}

#[test]
fn test_compress_level_zero_disables_compression() {
    let args = parse_args(["oc-rsync", "--compress-level=0", "src", "dest"]).unwrap();
    assert!(
        !args.compress,
        "--compress-level=0 should disable compression"
    );
    assert_eq!(args.compress_level, Some("0".into()));
}

#[test]
fn test_no_compression_by_default() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.compress, "Compression should be disabled by default");
    assert!(!args.no_compress);
    assert_eq!(args.compress_level, None);
}

// ============================================================================
// Partial Mode Activation
// ============================================================================

#[test]
fn test_partial_flag_alone() {
    let args = parse_args(["oc-rsync", "--partial", "src", "dest"]).unwrap();
    assert!(args.partial, "--partial flag should enable partial mode");
    assert_eq!(args.partial_dir, None);
}

#[test]
fn test_partial_dir_implies_partial() {
    let args = parse_args(["oc-rsync", "--partial-dir=/tmp/.rsync", "src", "dest"]).unwrap();
    assert!(
        args.partial,
        "--partial-dir should implicitly enable partial mode"
    );
    assert_eq!(args.partial_dir, Some("/tmp/.rsync".into()));
}

#[test]
fn test_no_partial_disables_partial() {
    let args = parse_args(["oc-rsync", "--partial", "--no-partial", "src", "dest"]).unwrap();
    assert!(!args.partial, "--no-partial should override --partial");
}

#[test]
fn test_no_partial_overrides_partial_dir() {
    let args = parse_args([
        "oc-rsync",
        "--partial-dir=/tmp",
        "--no-partial",
        "src",
        "dest",
    ])
    .unwrap();
    assert!(!args.partial, "--no-partial should override --partial-dir");
    assert_eq!(
        args.partial_dir, None,
        "--no-partial should clear partial_dir"
    );
}

#[test]
fn test_partial_disabled_by_default() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.partial, "Partial mode should be disabled by default");
    assert_eq!(args.partial_dir, None);
}

// ============================================================================
// Open-noatime Flag Override
// ============================================================================

#[test]
fn test_open_noatime_flag_alone() {
    let args = parse_args(["oc-rsync", "--open-noatime", "src", "dest"]).unwrap();
    assert!(args.open_noatime, "--open-noatime should be enabled");
    assert!(!args.no_open_noatime);
}

#[test]
fn test_no_open_noatime_overrides() {
    let args = parse_args([
        "oc-rsync",
        "--open-noatime",
        "--no-open-noatime",
        "src",
        "dest",
    ])
    .unwrap();
    assert!(
        !args.open_noatime,
        "--no-open-noatime should override --open-noatime"
    );
    assert!(args.no_open_noatime);
}

#[test]
fn test_open_noatime_disabled_by_default() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(
        !args.open_noatime,
        "open-noatime should be disabled by default"
    );
    assert!(!args.no_open_noatime);
}
