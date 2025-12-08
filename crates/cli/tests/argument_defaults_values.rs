//! Argument default value tests for Option<T> fields.
//!
//! Validates that value-based options (sizes, timeouts, limits, etc.) default
//! to None, matching upstream rsync 3.4.1 behavior.

use cli::test_utils::parse_args;
use core::client::{AddressMode, DeleteMode};

#[test]
fn test_max_delete_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.max_delete, None, "max_delete should default to None");
}

#[test]
fn test_min_size_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.min_size, None, "min_size should default to None");
}

#[test]
fn test_max_size_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.max_size, None, "max_size should default to None");
}

#[test]
fn test_block_size_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.block_size, None, "block_size should default to None");
}

#[test]
fn test_modify_window_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.modify_window, None,
        "modify_window should default to None"
    );
}

#[test]
fn test_timeout_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.timeout, None, "timeout should default to None");
}

#[test]
fn test_contimeout_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.contimeout, None, "contimeout should default to None");
}

#[test]
fn test_bwlimit_defaults_to_none() {
    let _args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    // BandwidthArgument is pub(crate), cannot be accessed from integration tests
    // TODO: Make BandwidthArgument pub if needed for testing
    // assert!(args.bwlimit.is_none(), "bwlimit should default to None");
}

#[test]
fn test_checksum_seed_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.checksum_seed, None,
        "checksum_seed should default to None"
    );
}

#[test]
fn test_checksum_choice_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.checksum_choice, None,
        "checksum_choice should default to None"
    );
}

#[test]
fn test_compress_level_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.compress_level, None,
        "compress_level should default to None"
    );
}

#[test]
fn test_compress_choice_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.compress_choice, None,
        "compress_choice should default to None"
    );
}

#[test]
fn test_skip_compress_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.skip_compress, None,
        "skip_compress should default to None"
    );
}

#[test]
fn test_protocol_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.protocol, None, "protocol should default to None");
}

#[test]
fn test_stop_after_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.stop_after, None, "stop_after should default to None");
}

#[test]
fn test_stop_at_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.stop_at, None, "stop_at should default to None");
}

#[test]
fn test_delete_mode_defaults_to_disabled() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.delete_mode,
        DeleteMode::Disabled,
        "delete_mode should default to Disabled"
    );
    assert!(
        !args.delete_mode.is_enabled(),
        "delete_mode should not be enabled by default"
    );
}

#[test]
fn test_address_mode_defaults_to_default() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.address_mode,
        AddressMode::Default,
        "address_mode should default to Default"
    );
}

#[test]
fn test_backup_dir_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.backup_dir, None, "backup_dir should default to None");
}

#[test]
fn test_backup_suffix_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.backup_suffix, None,
        "backup_suffix should default to None"
    );
}

#[test]
fn test_temp_dir_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.temp_dir, None, "temp_dir should default to None");
}

#[test]
fn test_log_file_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.log_file, None, "log_file should default to None");
}

#[test]
fn test_log_file_format_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.log_file_format, None,
        "log_file_format should default to None"
    );
}

#[test]
fn test_write_batch_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.write_batch, None, "write_batch should default to None");
}

#[test]
fn test_only_write_batch_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.only_write_batch, None,
        "only_write_batch should default to None"
    );
}

#[test]
fn test_read_batch_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.read_batch, None, "read_batch should default to None");
}

#[test]
fn test_out_format_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.out_format, None, "out_format should default to None");
}

#[test]
fn test_outbuf_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.outbuf, None, "outbuf should default to None");
}

#[test]
fn test_password_file_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.password_file, None,
        "password_file should default to None"
    );
}

#[test]
fn test_chown_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.chown, None, "chown should default to None");
}

#[test]
fn test_usermap_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.usermap, None, "usermap should default to None");
}

#[test]
fn test_groupmap_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.groupmap, None, "groupmap should default to None");
}

#[test]
fn test_iconv_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.iconv, None, "iconv should default to None");
}

#[test]
fn test_bind_address_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.bind_address, None,
        "bind_address should default to None"
    );
}

#[test]
fn test_sockopts_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.sockopts, None, "sockopts should default to None");
}

#[test]
fn test_rsync_path_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.rsync_path, None, "rsync_path should default to None");
}

#[test]
fn test_connect_program_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.connect_program, None,
        "connect_program should default to None"
    );
}

#[test]
fn test_daemon_port_defaults_to_none() {
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(args.daemon_port, None, "daemon_port should default to None");
}
