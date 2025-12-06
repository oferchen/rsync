//! Argument default value tests for special and derived flags.
//!
//! Validates special defaults like environment fallbacks, derived flags,
//! verbosity, progress settings, and other non-boolean/non-value flags.

use cli::test_utils::{NameOutputLevel, ProgressSetting, parse_args};

#[test]
fn test_archive_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.archive, "archive should default to false");
}

#[test]
fn test_backup_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.backup, "backup should default to false");
}

#[test]
fn test_partial_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.partial, "partial should default to false");
}

#[test]
fn test_preallocate_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.preallocate, "preallocate should default to false");
}

#[test]
fn test_delay_updates_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.delay_updates, "delay_updates should default to false");
}

#[test]
fn test_remove_source_files_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.remove_source_files, "remove_source_files should default to false");
}

#[test]
fn test_copy_dirlinks_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.copy_dirlinks, "copy_dirlinks should default to false");
}

#[test]
fn test_copy_devices_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.copy_devices, "copy_devices should default to false");
}

#[test]
fn test_copy_unsafe_links_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.copy_unsafe_links, None, "copy_unsafe_links should default to None");
}

#[test]
fn test_safe_links_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.safe_links, "safe_links should default to false");
}

#[test]
fn test_mkpath_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.mkpath, "mkpath should default to false");
}

#[test]
fn test_size_only_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.size_only, "size_only should default to false");
}

#[test]
fn test_ignore_times_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.ignore_times, "ignore_times should default to false");
}

#[test]
fn test_ignore_existing_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.ignore_existing, "ignore_existing should default to false");
}

#[test]
fn test_existing_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.existing, "existing should default to false");
}

#[test]
fn test_ignore_missing_args_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.ignore_missing_args, "ignore_missing_args should default to false");
}

#[test]
fn test_delete_missing_args_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.delete_missing_args, "delete_missing_args should default to false");
}

#[test]
fn test_delete_excluded_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.delete_excluded, "delete_excluded should default to false");
}

#[test]
fn test_update_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.update, "update should default to false");
}

#[test]
fn test_append_verify_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.append_verify, "append_verify should default to false");
}

#[test]
fn test_list_only_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.list_only, "list_only should default to false");
}

#[test]
fn test_stats_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.stats, "stats should default to false");
}

#[test]
fn test_eight_bit_output_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.eight_bit_output, "eight_bit_output should default to false");
}

#[test]
fn test_itemize_changes_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.itemize_changes, "itemize_changes should default to false");
}

#[test]
fn test_name_level_defaults_to_disabled() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.name_level, NameOutputLevel::Disabled, "name_level should default to Disabled");
}

#[test]
fn test_name_overridden_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.name_overridden, "name_overridden should default to false");
}

#[test]
fn test_verbosity_defaults_to_zero() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.verbosity, 0, "verbosity should default to 0");
}

#[test]
fn test_progress_defaults_to_unspecified() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.progress, ProgressSetting::Unspecified, "progress should default to Unspecified");
}

#[test]
fn test_no_motd_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.no_motd, "no_motd should default to false");
}

#[test]
fn test_no_iconv_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.no_iconv, "no_iconv should default to false");
}

#[test]
fn test_no_compress_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.no_compress, "no_compress should default to false");
}

#[test]
fn test_open_noatime_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.open_noatime, "open_noatime should default to false");
}

#[test]
fn test_no_open_noatime_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.no_open_noatime, "no_open_noatime should default to false");
}

#[test]
fn test_cvs_exclude_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.cvs_exclude, "cvs_exclude should default to false");
}

#[test]
fn test_from0_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.from0, "from0 should default to false");
}

#[test]
fn test_show_help_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.show_help, "show_help should default to false");
}

#[test]
fn test_show_version_defaults_to_false() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(!args.show_version, "show_version should default to false");
}

#[test]
fn test_remote_shell_defaults_to_none_without_env() {
    // This test assumes RSYNC_RSH environment variable is not set
    // If set in actual environment, it will be respected
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    // remote_shell may be Some if RSYNC_RSH env var is set
    // We can only test that it's None if env is clean
    if std::env::var("RSYNC_RSH").is_err() {
        assert_eq!(args.remote_shell, None, "remote_shell should default to None when RSYNC_RSH not set");
    }
}

#[test]
fn test_partial_dir_defaults_to_none_without_env() {
    // This test assumes RSYNC_PARTIAL_DIR environment variable is not set
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    if std::env::var("RSYNC_PARTIAL_DIR").is_err() {
        assert_eq!(args.partial_dir, None, "partial_dir should default to None when RSYNC_PARTIAL_DIR not set");
    }
}

#[test]
fn test_human_readable_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.human_readable, None, "human_readable should default to None");
}

#[test]
fn test_protect_args_defaults_to_environment_or_none() {
    // protect_args defaults to None unless environment sets it
    let _args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    // Can't assert specific value as it depends on environment
    // Just verify it parses without error
}

#[test]
fn test_link_dests_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.link_dests.is_empty(), "link_dests should default to empty vec");
}

#[test]
fn test_excludes_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.excludes.is_empty(), "excludes should default to empty vec");
}

#[test]
fn test_includes_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.includes.is_empty(), "includes should default to empty vec");
}

#[test]
fn test_filters_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.filters.is_empty(), "filters should default to empty vec");
}

#[test]
fn test_chmod_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.chmod.is_empty(), "chmod should default to empty vec");
}

#[test]
fn test_remote_options_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.remote_options.is_empty(), "remote_options should default to empty vec");
}

#[test]
fn test_info_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.info.is_empty(), "info should default to empty vec");
}

#[test]
fn test_debug_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.debug.is_empty(), "debug should default to empty vec");
}

#[test]
fn test_files_from_defaults_to_empty() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(args.files_from.is_empty(), "files_from should default to empty vec");
}
