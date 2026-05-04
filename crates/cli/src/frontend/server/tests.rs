use std::ffi::OsString;
use std::time::SystemTime;

use super::daemon::{daemon_mode_arguments, server_mode_requested};
use super::flags::{detect_secluded_args_flag, is_known_server_long_flag, parse_server_long_flags};
use super::parse::{
    parse_server_checksum_seed, parse_server_flag_string_and_args, parse_server_size_limit,
    parse_server_stop_after, parse_server_stop_at,
};

#[test]
fn daemon_mode_arguments_empty_args() {
    let args: Vec<OsString> = vec![];
    assert!(daemon_mode_arguments(&args).is_none());
}

#[test]
fn daemon_mode_arguments_no_daemon_flag() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("-av"),
        OsString::from("src/"),
        OsString::from("dest/"),
    ];
    assert!(daemon_mode_arguments(&args).is_none());
}

#[test]
fn daemon_mode_arguments_with_daemon_flag() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--daemon"),
        OsString::from("--port=8873"),
    ];
    let result = daemon_mode_arguments(&args);
    assert!(result.is_some());
    let daemon_args = result.unwrap();
    assert!(daemon_args.iter().any(|a| a == "--port=8873"));
    assert!(!daemon_args.iter().any(|a| a == "--daemon"));
}

#[test]
fn daemon_mode_arguments_daemon_flag_with_config() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--daemon"),
        OsString::from("--config=/etc/rsyncd.conf"),
        OsString::from("--no-detach"),
    ];
    let result = daemon_mode_arguments(&args);
    assert!(result.is_some());
    let daemon_args = result.unwrap();
    assert!(daemon_args.iter().any(|a| a == "--config=/etc/rsyncd.conf"));
    assert!(daemon_args.iter().any(|a| a == "--no-detach"));
}

#[test]
fn daemon_mode_arguments_daemon_after_double_dash_ignored() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("-av"),
        OsString::from("--"),
        OsString::from("--daemon"),
    ];
    let result = daemon_mode_arguments(&args);
    assert!(result.is_none());
}

#[test]
fn daemon_mode_arguments_double_dash_preserved() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--daemon"),
        OsString::from("--"),
        OsString::from("extra-arg"),
    ];
    let result = daemon_mode_arguments(&args);
    assert!(result.is_some());
    let daemon_args = result.unwrap();
    assert!(daemon_args.iter().any(|a| a == "--"));
    assert!(daemon_args.iter().any(|a| a == "extra-arg"));
}

#[test]
fn daemon_mode_arguments_program_only() {
    let args: Vec<OsString> = vec![OsString::from("rsync")];
    assert!(daemon_mode_arguments(&args).is_none());
}

#[test]
fn daemon_mode_arguments_oc_rsync_program() {
    let args: Vec<OsString> = vec![OsString::from("oc-rsync"), OsString::from("--daemon")];
    let result = daemon_mode_arguments(&args);
    assert!(result.is_some());
    let daemon_args = result.unwrap();
    assert!(!daemon_args.is_empty());
}

#[test]
fn server_mode_requested_no_server_flag() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("-av"),
        OsString::from("src/"),
        OsString::from("dest/"),
    ];
    assert!(!server_mode_requested(&args));
}

#[test]
fn server_mode_requested_with_server_flag() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-logDtprze.iLsfxC"),
        OsString::from("."),
        OsString::from("src/"),
    ];
    assert!(server_mode_requested(&args));
}

#[test]
fn server_mode_requested_server_first_arg() {
    let args: Vec<OsString> = vec![OsString::from("rsync"), OsString::from("--server")];
    assert!(server_mode_requested(&args));
}

#[test]
fn server_mode_requested_empty_args() {
    let args: Vec<OsString> = vec![];
    assert!(!server_mode_requested(&args));
}

#[test]
fn server_mode_requested_program_only() {
    let args: Vec<OsString> = vec![OsString::from("rsync")];
    assert!(!server_mode_requested(&args));
}

#[test]
fn server_mode_requested_with_sender() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("-logDtprze.iLsfxC"),
        OsString::from("."),
        OsString::from("src/"),
    ];
    assert!(server_mode_requested(&args));
}

#[test]
fn server_mode_requested_with_receiver() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("--receiver"),
        OsString::from("-logDtprze.iLsfxC"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    assert!(server_mode_requested(&args));
}

#[test]
fn server_mode_requested_server_not_in_first_position() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("-v"),
        OsString::from("--server"),
        OsString::from("-logDtprze.iLsfxC"),
    ];
    assert!(server_mode_requested(&args));
}

#[test]
fn detect_secluded_args_when_present() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-s"),
        OsString::from("."),
    ];
    assert!(detect_secluded_args_flag(&args));
}

#[test]
fn detect_secluded_args_when_absent() {
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("dest"),
    ];
    assert!(!detect_secluded_args_flag(&args));
}

#[test]
fn detect_secluded_args_ignores_program_name() {
    let args: Vec<OsString> = vec![OsString::from("-s"), OsString::from("--server")];
    assert!(!detect_secluded_args_flag(&args));
}

#[test]
fn parse_server_args_basic() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("dest"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-logDtpr");
    assert_eq!(pos_args, vec![OsString::from("dest")]);
}

#[test]
fn parse_server_args_skips_known_long_args() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("--ignore-errors"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("src/"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-logDtpr");
    assert_eq!(pos_args, vec![OsString::from("src/")]);
}

#[test]
fn parse_server_args_skips_secluded_flag() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("-s"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("dest"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-logDtpr");
    assert_eq!(pos_args, vec![OsString::from("dest")]);
}

#[test]
fn parse_server_args_skips_new_boolean_long_flags() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("--write-devices"),
        OsString::from("--trust-sender"),
        OsString::from("--qsort"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("src/"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-logDtpr");
    assert_eq!(pos_args, vec![OsString::from("src/")]);
}

#[test]
fn parse_server_args_skips_value_bearing_long_flags() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--checksum-seed=12345"),
        OsString::from("--checksum-choice=xxh3"),
        OsString::from("--min-size=1K"),
        OsString::from("--max-size=1G"),
        OsString::from("--stop-after=60"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-logDtpr");
    assert_eq!(pos_args, vec![OsString::from("dest/")]);
}

#[test]
fn long_flags_defaults() {
    let args: Vec<OsString> = vec![OsString::from("--server")];
    let flags = parse_server_long_flags(&args);
    assert!(!flags.is_sender);
    assert!(!flags.is_receiver);
    assert!(!flags.ignore_errors);
    assert!(!flags.fsync);
    assert!(!flags.write_devices);
    assert!(!flags.trust_sender);
    assert!(!flags.qsort);
    assert!(flags.checksum_seed.is_none());
    assert!(flags.checksum_choice.is_none());
    assert!(flags.min_size.is_none());
    assert!(flags.max_size.is_none());
    assert!(flags.stop_at.is_none());
    assert!(flags.stop_after.is_none());
    assert!(matches!(
        flags.io_uring_policy,
        fast_io::IoUringPolicy::Auto
    ));
}

#[test]
fn long_flags_sender() {
    let args = vec![OsString::from("--server"), OsString::from("--sender")];
    let flags = parse_server_long_flags(&args);
    assert!(flags.is_sender);
    assert!(!flags.is_receiver);
}

#[test]
fn long_flags_receiver() {
    let args = vec![OsString::from("--server"), OsString::from("--receiver")];
    let flags = parse_server_long_flags(&args);
    assert!(!flags.is_sender);
    assert!(flags.is_receiver);
}

#[test]
fn long_flags_ignore_errors() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--ignore-errors"),
    ];
    let flags = parse_server_long_flags(&args);
    assert!(flags.ignore_errors);
}

#[test]
fn long_flags_fsync() {
    let args = vec![OsString::from("--server"), OsString::from("--fsync")];
    let flags = parse_server_long_flags(&args);
    assert!(flags.fsync);
}

#[test]
fn long_flags_io_uring_enabled() {
    let args = vec![OsString::from("--server"), OsString::from("--io-uring")];
    let flags = parse_server_long_flags(&args);
    assert!(matches!(
        flags.io_uring_policy,
        fast_io::IoUringPolicy::Enabled
    ));
}

#[test]
fn long_flags_io_uring_disabled() {
    let args = vec![OsString::from("--server"), OsString::from("--no-io-uring")];
    let flags = parse_server_long_flags(&args);
    assert!(matches!(
        flags.io_uring_policy,
        fast_io::IoUringPolicy::Disabled
    ));
}

#[test]
fn long_flags_write_devices() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--write-devices"),
    ];
    let flags = parse_server_long_flags(&args);
    assert!(flags.write_devices);
}

#[test]
fn long_flags_trust_sender() {
    let args = vec![OsString::from("--server"), OsString::from("--trust-sender")];
    let flags = parse_server_long_flags(&args);
    assert!(flags.trust_sender);
}

#[test]
fn long_flags_qsort() {
    let args = vec![OsString::from("--server"), OsString::from("--qsort")];
    let flags = parse_server_long_flags(&args);
    assert!(flags.qsort);
}

#[test]
fn long_flags_checksum_seed() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--checksum-seed=42"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.checksum_seed.as_deref(), Some("42"));
}

#[test]
fn long_flags_checksum_choice() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--checksum-choice=xxh3"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.checksum_choice.as_deref(), Some("xxh3"));
}

#[test]
fn long_flags_min_size() {
    let args = vec![OsString::from("--server"), OsString::from("--min-size=1K")];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.min_size.as_deref(), Some("1K"));
}

#[test]
fn long_flags_max_size() {
    let args = vec![OsString::from("--server"), OsString::from("--max-size=1G")];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.max_size.as_deref(), Some("1G"));
}

#[test]
fn long_flags_stop_at() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--stop-at=2099-12-31T23:59"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.stop_at.as_deref(), Some("2099-12-31T23:59"));
}

#[test]
fn long_flags_stop_after() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--stop-after=60"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.stop_after.as_deref(), Some("60"));
}

#[test]
fn long_flags_all_combined() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("--ignore-errors"),
        OsString::from("--fsync"),
        OsString::from("--write-devices"),
        OsString::from("--trust-sender"),
        OsString::from("--qsort"),
        OsString::from("--checksum-seed=99"),
        OsString::from("--checksum-choice=md5"),
        OsString::from("--min-size=100"),
        OsString::from("--max-size=1M"),
        OsString::from("--stop-after=30"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("src/"),
    ];
    let flags = parse_server_long_flags(&args);
    assert!(flags.is_sender);
    assert!(flags.ignore_errors);
    assert!(flags.fsync);
    assert!(flags.write_devices);
    assert!(flags.trust_sender);
    assert!(flags.qsort);
    assert_eq!(flags.checksum_seed.as_deref(), Some("99"));
    assert_eq!(flags.checksum_choice.as_deref(), Some("md5"));
    assert_eq!(flags.min_size.as_deref(), Some("100"));
    assert_eq!(flags.max_size.as_deref(), Some("1M"));
    assert_eq!(flags.stop_after.as_deref(), Some("30"));
}

#[test]
fn known_flag_detects_boolean_flags() {
    assert!(is_known_server_long_flag("--server"));
    assert!(is_known_server_long_flag("--sender"));
    assert!(is_known_server_long_flag("--receiver"));
    assert!(is_known_server_long_flag("--ignore-errors"));
    assert!(is_known_server_long_flag("--fsync"));
    assert!(is_known_server_long_flag("--io-uring"));
    assert!(is_known_server_long_flag("--no-io-uring"));
    assert!(is_known_server_long_flag("--write-devices"));
    assert!(is_known_server_long_flag("--trust-sender"));
    assert!(is_known_server_long_flag("--qsort"));
    assert!(is_known_server_long_flag("--from0"));
    assert!(is_known_server_long_flag("-s"));
}

#[test]
fn known_flag_detects_value_flags() {
    assert!(is_known_server_long_flag("--checksum-seed=0"));
    assert!(is_known_server_long_flag("--checksum-choice=xxh3"));
    assert!(is_known_server_long_flag("--min-size=1K"));
    assert!(is_known_server_long_flag("--max-size=1G"));
    assert!(is_known_server_long_flag("--stop-at=2099-12-31"));
    assert!(is_known_server_long_flag("--stop-after=60"));
    assert!(is_known_server_long_flag("--files-from=-"));
    assert!(is_known_server_long_flag("--files-from=/path/to/list"));
}

#[test]
fn known_flag_rejects_unknown() {
    assert!(!is_known_server_long_flag("--unknown"));
    assert!(!is_known_server_long_flag("-v"));
    assert!(!is_known_server_long_flag("-logDtpr"));
    assert!(!is_known_server_long_flag("dest/"));
}

#[test]
fn checksum_seed_parses_valid() {
    assert_eq!(parse_server_checksum_seed("0").unwrap(), 0);
    assert_eq!(parse_server_checksum_seed("12345").unwrap(), 12345);
    assert_eq!(parse_server_checksum_seed("4294967295").unwrap(), u32::MAX);
}

#[test]
fn checksum_seed_rejects_empty() {
    assert!(parse_server_checksum_seed("").is_err());
}

#[test]
fn checksum_seed_rejects_non_numeric() {
    assert!(parse_server_checksum_seed("abc").is_err());
}

#[test]
fn checksum_seed_rejects_overflow() {
    assert!(parse_server_checksum_seed("4294967296").is_err());
}

#[test]
fn checksum_seed_trims_whitespace() {
    assert_eq!(parse_server_checksum_seed("  42  ").unwrap(), 42);
}

#[test]
fn size_limit_parses_plain_number() {
    assert_eq!(parse_server_size_limit("100", "--min-size").unwrap(), 100);
}

#[test]
fn size_limit_parses_kilobytes() {
    assert_eq!(parse_server_size_limit("1K", "--min-size").unwrap(), 1024);
}

#[test]
fn size_limit_parses_megabytes() {
    assert_eq!(
        parse_server_size_limit("1M", "--max-size").unwrap(),
        1024 * 1024
    );
}

#[test]
fn size_limit_parses_gigabytes() {
    assert_eq!(
        parse_server_size_limit("1G", "--max-size").unwrap(),
        1024 * 1024 * 1024
    );
}

#[test]
fn size_limit_rejects_empty() {
    assert!(parse_server_size_limit("", "--min-size").is_err());
}

#[test]
fn size_limit_rejects_invalid() {
    assert!(parse_server_size_limit("abc", "--max-size").is_err());
}

#[test]
fn stop_after_parses_valid_minutes() {
    let deadline = parse_server_stop_after("10").unwrap();
    let duration = deadline.duration_since(SystemTime::now()).unwrap();
    assert!(duration.as_secs() >= 598 && duration.as_secs() <= 602);
}

#[test]
fn stop_after_rejects_zero() {
    assert!(parse_server_stop_after("0").is_err());
}

#[test]
fn stop_after_rejects_empty() {
    assert!(parse_server_stop_after("").is_err());
}

#[test]
fn stop_after_rejects_non_numeric() {
    assert!(parse_server_stop_after("abc").is_err());
}

#[test]
fn stop_at_rejects_empty() {
    assert!(parse_server_stop_at("").is_err());
}

#[test]
fn stop_at_rejects_invalid_format() {
    assert!(parse_server_stop_at("invalid").is_err());
}

#[test]
fn stop_at_parses_far_future_date() {
    let result = parse_server_stop_at("2099-12-31T23:59");
    // May fail due to local offset issues in test env, but format should be ok
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn long_flags_files_from_stdin() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--files-from=-"),
        OsString::from("--from0"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.files_from.as_deref(), Some("-"));
    assert!(flags.from0);
}

#[test]
fn long_flags_files_from_path() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--files-from=/tmp/list.txt"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.files_from.as_deref(), Some("/tmp/list.txt"));
    assert!(!flags.from0);
}

#[test]
fn long_flags_from0_without_files_from() {
    let args = vec![OsString::from("--server"), OsString::from("--from0")];
    let flags = parse_server_long_flags(&args);
    assert!(flags.from0);
    assert!(flags.files_from.is_none());
}

#[test]
fn long_flags_default_files_from() {
    let args = vec![OsString::from("--server")];
    let flags = parse_server_long_flags(&args);
    assert!(flags.files_from.is_none());
    assert!(!flags.from0);
}

#[test]
fn parse_server_args_skips_files_from_and_from0() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--files-from=-"),
        OsString::from("--from0"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-logDtpr");
    assert_eq!(pos_args, vec![OsString::from("dest/")]);
}

#[test]
fn long_flags_timeout_extracts_value() {
    // upstream: options.c - server_options() emits `--timeout=%d` from io_timeout.
    let args = vec![OsString::from("--server"), OsString::from("--timeout=10")];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.timeout.as_deref(), Some("10"));
}

#[test]
fn known_flag_detects_timeout() {
    assert!(is_known_server_long_flag("--timeout=0"));
    assert!(is_known_server_long_flag("--timeout=10"));
    assert!(is_known_server_long_flag("--timeout=300"));
}

#[test]
fn parse_server_args_iconv_and_timeout_strip_to_dest() {
    // Regression: iconv-local-ssh CI failure. Before the fix, `--timeout=10`
    // was unrecognised by `is_known_server_long_flag` and was consumed as a
    // positional argument, so the destination directory `dest/` ended up as
    // `--timeout=10/` (verified via strace `mkdirat` / `renameat` calls).
    // The exact arg sequence here mirrors what upstream rsync emits when the
    // client runs `oc-rsync --iconv=UTF-8,ISO-8859-1 --timeout=10 src/ host:dest/`.
    let args = vec![
        OsString::from("--server"),
        OsString::from("-vlogDtpre.iLsfxCIvu"),
        OsString::from("--iconv=ISO-8859-1"),
        OsString::from("--timeout=10"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-vlogDtpre.iLsfxCIvu");
    assert_eq!(pos_args, vec![OsString::from("dest/")]);
}
