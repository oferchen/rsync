//! Argument default value tests for boolean flags.
//!
//! Validates that boolean preservation and transfer mode flags default
//! to their correct upstream rsync 3.4.1 values.

use cli::test_utils::parse_args;

#[test]
fn test_preserve_owner_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.owner, None, "owner should default to None (disabled unless -a)");
}

#[test]
fn test_preserve_group_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.group, None, "group should default to None (disabled unless -a)");
}

#[test]
fn test_preserve_permissions_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.perms, None, "perms should default to None (disabled unless -a)");
}

#[test]
fn test_preserve_times_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.times, None, "times should default to None (disabled unless -a)");
}

#[test]
fn test_preserve_devices_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.devices, None, "devices should default to None (disabled unless -a)");
}

#[test]
fn test_preserve_specials_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.specials, None, "specials should default to None (disabled unless -a)");
}

#[test]
fn test_preserve_hard_links_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.hard_links, None, "hard_links should default to None");
}

#[test]
fn test_preserve_symlinks_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.links, None, "links should default to None (disabled unless -a)");
}

#[test]
fn test_preserve_acls_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.acls, None, "acls should default to None");
}

#[test]
fn test_preserve_xattrs_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.xattrs, None, "xattrs should default to None");
}

#[test]
fn test_recursive_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.recursive, "recursive should default to false");
}

#[test]
fn test_compress_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.compress, "compress should default to false");
}

#[test]
fn test_dry_run_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.dry_run, "dry_run should default to false");
}

#[test]
fn test_checksum_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.checksum, None, "checksum should default to None");
}

#[test]
fn test_sparse_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.sparse, None, "sparse should default to None");
}

#[test]
fn test_dirs_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.dirs, None, "dirs should default to None");
}

#[test]
fn test_fuzzy_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.fuzzy, None, "fuzzy should default to None");
}

#[test]
fn test_copy_links_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.copy_links, None, "copy_links should default to None");
}

#[test]
fn test_keep_dirlinks_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.keep_dirlinks, None, "keep_dirlinks should default to None");
}

#[test]
fn test_force_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.force, None, "force should default to None");
}

#[test]
fn test_relative_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.relative, None, "relative should default to None");
}

#[test]
fn test_one_file_system_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.one_file_system, None, "one_file_system should default to None");
}

#[test]
fn test_implied_dirs_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.implied_dirs, None, "implied_dirs should default to None");
}

#[test]
fn test_prune_empty_dirs_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.prune_empty_dirs, None, "prune_empty_dirs should default to None");
}

#[test]
fn test_fsync_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.fsync, None, "fsync should default to None");
}

#[test]
fn test_inplace_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.inplace, None, "inplace should default to None");
}

#[test]
fn test_whole_file_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.whole_file, None, "whole_file should default to None");
}

#[test]
fn test_append_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.append, None, "append should default to None");
}

#[test]
fn test_super_mode_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.super_mode, None, "super_mode should default to None");
}

#[test]
fn test_numeric_ids_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.numeric_ids, None, "numeric_ids should default to None");
}

#[test]
fn test_omit_dir_times_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.omit_dir_times, None, "omit_dir_times should default to None");
}

#[test]
fn test_omit_link_times_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.omit_link_times, None, "omit_link_times should default to None");
}

#[test]
fn test_write_devices_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.write_devices, None, "write_devices should default to None");
}

#[test]
fn test_executability_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.executability, None, "executability should default to None");
}

#[test]
fn test_inc_recursive_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.inc_recursive, None, "inc_recursive should default to None");
}

#[test]
fn test_blocking_io_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.blocking_io, None, "blocking_io should default to None");
}

#[test]
fn test_msgs_to_stderr_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.msgs_to_stderr, None, "msgs_to_stderr should default to None");
}
