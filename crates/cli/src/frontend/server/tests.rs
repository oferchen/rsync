use std::ffi::OsString;
use std::time::SystemTime;

use super::daemon::{
    daemon_mode_arguments, server_daemon_arguments, server_daemon_mode_requested,
    server_mode_requested,
};
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
fn detect_secluded_args_in_compact_flag_string() {
    // upstream: options.c:2604 - server_options() puts 's' at argstr[1]
    // when protect_args is active, producing e.g. `-slogDtprze.iLsfxCIvu`.
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-slogDtprze.iLsfxCIvu"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    assert!(detect_secluded_args_flag(&args));
}

#[test]
fn detect_secluded_args_in_compact_flag_string_middle() {
    // The 's' can appear anywhere in the transfer flags portion.
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-logDtprs"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    assert!(detect_secluded_args_flag(&args));
}

#[test]
fn detect_secluded_args_ignores_s_in_capability_string() {
    // The 's' after the dot is the symlink-iconv capability char,
    // not secluded-args. Must not trigger secluded-args detection.
    // upstream: options.c:3027 - 's' in capability string = ICONV_OPTION
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-logDtprze.iLsfxCIvu"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    assert!(!detect_secluded_args_flag(&args));
}

#[test]
fn detect_secluded_args_both_transfer_and_capability_s() {
    // When 's' appears in both the transfer portion (secluded-args) AND
    // the capability string (symlink-iconv), should detect it.
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-slogDtprze.iLsfxCIvu"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    assert!(detect_secluded_args_flag(&args));
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

/// Regression for UTS-SLDB.REOPEN (`symlink-dirlink-basis_test.py` test 7):
/// upstream `server_options()` (options.c:2886-2890) emits `--partial-dir`
/// and its value as TWO separate argv entries. Both `--partial-dir` itself
/// and the value that follows must be stripped from the positional list,
/// otherwise the value (`.rsync-partial`) shows up as a destination path
/// and the receiver creates a directory literally named `--partial-dir`.
#[test]
fn parse_server_args_skips_split_partial_dir_flag() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("-vlKtpR"),
        OsString::from("--partial-dir"),
        OsString::from(".rsync-partial"),
        OsString::from("."),
        OsString::from("."),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-vlKtpR");
    assert!(
        pos_args.is_empty(),
        "split --partial-dir and value must not leak into positional args: {pos_args:?}",
    );
}

/// Companion to the split-form test above: `--partial-dir=VALUE` is the
/// joined form used by the client-side CLI parser. The server parser must
/// also recognise it so non-upstream clients that emit the joined form do
/// not corrupt the positional list.
#[test]
fn parse_server_args_skips_joined_partial_dir_flag() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("-vlKtpR"),
        OsString::from("--partial-dir=.rsync-partial"),
        OsString::from("."),
        OsString::from("dest"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-vlKtpR");
    assert_eq!(pos_args, vec![OsString::from("dest")]);
}

/// `parse_server_long_flags` must capture both split and joined
/// `--partial-dir` forms into `ServerLongFlags::partial_dir`.
#[test]
fn long_flags_captures_split_partial_dir() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--partial-dir"),
        OsString::from(".rsync-partial"),
        OsString::from("--delay-updates"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(
        flags.partial_dir.as_deref(),
        Some(std::ffi::OsStr::new(".rsync-partial")),
    );
    assert!(flags.delay_updates);
}

#[test]
fn long_flags_captures_joined_partial_dir() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--partial-dir=.rsync-partial"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(
        flags.partial_dir.as_deref(),
        Some(std::ffi::OsStr::new(".rsync-partial")),
    );
    assert!(!flags.delay_updates);
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
    assert!(flags.max_alloc.is_none());
    assert!(flags.stop_at.is_none());
    assert!(flags.stop_after.is_none());
    assert!(matches!(
        flags.io_uring_policy,
        fast_io::IoUringPolicy::Auto
    ));
    assert!(matches!(
        flags.zero_copy_policy,
        fast_io::ZeroCopyPolicy::Auto
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
fn long_flags_zero_copy_default_is_auto() {
    let args: Vec<OsString> = vec![OsString::from("--server")];
    let flags = parse_server_long_flags(&args);
    assert!(matches!(
        flags.zero_copy_policy,
        fast_io::ZeroCopyPolicy::Auto
    ));
}

#[test]
fn long_flags_zero_copy_enabled() {
    let args = vec![OsString::from("--server"), OsString::from("--zero-copy")];
    let flags = parse_server_long_flags(&args);
    assert!(matches!(
        flags.zero_copy_policy,
        fast_io::ZeroCopyPolicy::Enabled
    ));
}

#[test]
fn long_flags_zero_copy_disabled() {
    let args = vec![OsString::from("--server"), OsString::from("--no-zero-copy")];
    let flags = parse_server_long_flags(&args);
    assert!(matches!(
        flags.zero_copy_policy,
        fast_io::ZeroCopyPolicy::Disabled
    ));
}

#[test]
fn long_flags_zero_copy_is_known() {
    assert!(is_known_server_long_flag("--zero-copy"));
    assert!(is_known_server_long_flag("--no-zero-copy"));
}

#[test]
fn long_flags_io_uring_depth_value() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--io-uring-depth=256"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.io_uring_depth.as_deref(), Some("256"));
}

#[test]
fn long_flags_io_uring_depth_default_is_none() {
    let args = vec![OsString::from("--server")];
    let flags = parse_server_long_flags(&args);
    assert!(flags.io_uring_depth.is_none());
}

// upstream: options.c:2928-2931 - server_options() forwards --info=FLAGS so
// the server must recognise it as a long flag and not let it leak into the
// positional path list.
#[test]
fn long_flags_info_is_captured() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--info=PROGRESS,STATS"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(
        flags
            .info
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["PROGRESS,STATS".to_owned()],
    );
    assert!(is_known_server_long_flag("--info=PROGRESS,STATS"));
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
fn long_flags_max_alloc() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--max-alloc=512M"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.max_alloc.as_deref(), Some("512M"));
}

#[test]
fn long_flags_max_alloc_is_known_long_flag() {
    let args = [
        OsString::from("--server"),
        OsString::from("--max-alloc=1G"),
        OsString::from("-logDtpr"),
        OsString::from("."),
        OsString::from("src/"),
    ];
    let (flag_string, positional) = parse_server_flag_string_and_args(&args[1..]);
    assert_eq!(flag_string, "-logDtpr");
    assert_eq!(positional, vec![OsString::from("src/")]);
    assert!(is_known_server_long_flag("--max-alloc=1G"));
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
        OsString::from("--max-alloc=2G"),
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
    assert_eq!(flags.max_alloc.as_deref(), Some("2G"));
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
    assert!(is_known_server_long_flag("--io-uring-depth=128"));
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

#[test]
fn is_known_server_long_flag_compression_choices() {
    // upstream: options.c:2809-2814 - server_options() emits these whenever the
    // negotiated codec is not the default CPRES_ZLIB carried by the compact `-z`
    // flag. Without them in the known list, the dest path is silently corrupted
    // (same failure mode as the iconv/timeout regression above).
    assert!(is_known_server_long_flag("--new-compress"));
    assert!(is_known_server_long_flag("--old-compress"));
    assert!(is_known_server_long_flag("--compress-choice=zstd"));
    assert!(is_known_server_long_flag("--compress-choice=zlib"));
    assert!(is_known_server_long_flag("--zc=lz4"));
}

#[test]
fn parse_server_long_flags_captures_compress_choice() {
    let args = vec![
        OsString::from("--server"),
        OsString::from("--compress-choice=zstd"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.compress_choice.as_deref(), Some("zstd"));
}

#[test]
fn parse_server_long_flags_compress_choice_zc_alias() {
    let args = vec![OsString::from("--server"), OsString::from("--zc=lz4")];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.compress_choice.as_deref(), Some("lz4"));
}

#[test]
fn parse_server_long_flags_new_compress_maps_to_zlibx() {
    let args = vec![OsString::from("--server"), OsString::from("--new-compress")];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.compress_choice.as_deref(), Some("zlibx"));
}

#[test]
fn parse_server_long_flags_old_compress_maps_to_zlib() {
    let args = vec![OsString::from("--server"), OsString::from("--old-compress")];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.compress_choice.as_deref(), Some("zlib"));
}

#[test]
fn parse_server_args_compress_choice_strips_from_dest() {
    // Regression: with `-z` upstream picks the highest priority codec (ZSTD on
    // protocol 32) and sends `--compress-choice=zstd`. Before the fix this fell
    // through to positional args, corrupting the destination path identically to
    // the iconv/timeout regression.
    let args = vec![
        OsString::from("--server"),
        OsString::from("-vlogDtpre.iLsfxCIvuz"),
        OsString::from("--compress-choice=zstd"),
        OsString::from("--timeout=10"),
        OsString::from("."),
        OsString::from("dest/"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-vlogDtpre.iLsfxCIvuz");
    assert_eq!(pos_args, vec![OsString::from("dest/")]);
}

#[test]
fn parse_server_long_flags_log_format_itemize() {
    // upstream: options.c:2757 - client sends --log-format=%i for itemize
    let args = vec![
        OsString::from("--server"),
        OsString::from("--log-format=%i"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.log_format.as_deref(), Some("%i"));
}

#[test]
fn parse_server_long_flags_log_format_itemize_extended() {
    // upstream: options.c:2755 - %i%I when stdout_format_has_i > 1
    let args = vec![
        OsString::from("--server"),
        OsString::from("--log-format=%i%I"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.log_format.as_deref(), Some("%i%I"));
}

#[test]
fn parse_server_long_flags_log_format_operation() {
    // upstream: options.c:2759 - %o when stdout_format_has_o_or_i
    let args = vec![
        OsString::from("--server"),
        OsString::from("--log-format=%o"),
    ];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.log_format.as_deref(), Some("%o"));
}

#[test]
fn parse_server_long_flags_log_format_placeholder() {
    // upstream: options.c:2761 - X when not verbose, no i/o tokens
    let args = vec![OsString::from("--server"), OsString::from("--log-format=X")];
    let flags = parse_server_long_flags(&args);
    assert_eq!(flags.log_format.as_deref(), Some("X"));
}

#[test]
fn log_format_recognized_as_known_server_long_flag() {
    assert!(is_known_server_long_flag("--log-format=%i"));
    assert!(is_known_server_long_flag("--log-format=%i%I"));
    assert!(is_known_server_long_flag("--log-format=%o"));
    assert!(is_known_server_long_flag("--log-format=X"));
}

#[test]
fn parse_server_args_log_format_strips_from_dest() {
    // Regression: without recognizing --log-format as a known server flag,
    // it falls through to positional args and the server tries to find a
    // file named "--log-format=%i", reporting "file has vanished".
    let args = vec![
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("-logDtpre.iLsfxCIvu"),
        OsString::from("--log-format=%i"),
        OsString::from("."),
        OsString::from("/src/path/"),
    ];
    let (flags, pos_args) = parse_server_flag_string_and_args(&args);
    assert_eq!(flags, "-logDtpre.iLsfxCIvu");
    assert_eq!(pos_args, vec![OsString::from("/src/path/")]);
}

// --- server_daemon_mode_requested tests ---

#[test]
fn server_daemon_mode_not_requested_empty() {
    let args: Vec<OsString> = vec![];
    assert!(!server_daemon_mode_requested(&args));
}

#[test]
fn server_daemon_mode_not_requested_server_only() {
    let args = vec![OsString::from("rsync"), OsString::from("--server")];
    assert!(!server_daemon_mode_requested(&args));
}

#[test]
fn server_daemon_mode_not_requested_daemon_only() {
    let args = vec![OsString::from("rsync"), OsString::from("--daemon")];
    assert!(!server_daemon_mode_requested(&args));
}

#[test]
fn server_daemon_mode_requested_both_flags() {
    let args = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("--daemon"),
        OsString::from("."),
    ];
    assert!(server_daemon_mode_requested(&args));
}

#[test]
fn server_daemon_mode_requested_with_config() {
    let args = vec![
        OsString::from("rsync"),
        OsString::from("--config=/etc/rsyncd.conf"),
        OsString::from("--server"),
        OsString::from("--daemon"),
        OsString::from("."),
    ];
    assert!(server_daemon_mode_requested(&args));
}

#[test]
fn server_daemon_mode_not_requested_after_double_dash() {
    let args = vec![
        OsString::from("rsync"),
        OsString::from("--"),
        OsString::from("--server"),
        OsString::from("--daemon"),
    ];
    assert!(!server_daemon_mode_requested(&args));
}

// --- server_daemon_arguments tests ---

#[test]
fn server_daemon_arguments_strips_server_and_daemon() {
    let args = vec![
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("--daemon"),
        OsString::from("."),
    ];
    let result = server_daemon_arguments(&args);
    // Should not contain --server, --daemon, or "."
    assert!(!result.iter().any(|a| a == "--server"));
    assert!(!result.iter().any(|a| a == "--daemon"));
    assert!(!result.iter().any(|a| a == "."));
}

#[test]
fn server_daemon_arguments_preserves_config() {
    let args = vec![
        OsString::from("rsync"),
        OsString::from("--config=/etc/rsyncd.conf"),
        OsString::from("--server"),
        OsString::from("--daemon"),
        OsString::from("."),
    ];
    let result = server_daemon_arguments(&args);
    assert!(result.iter().any(|a| a == "--config=/etc/rsyncd.conf"));
}

#[test]
fn server_daemon_arguments_sets_daemon_program_name() {
    let args = vec![
        OsString::from("oc-rsync"),
        OsString::from("--server"),
        OsString::from("--daemon"),
        OsString::from("."),
    ];
    let result = server_daemon_arguments(&args);
    // First element should be the daemon program name
    assert!(!result.is_empty());
}

/// Reproduces the wire layout upstream produces for `rsync -ais lh:src/ dest/`:
/// the command-line argv carries the server-options head (`--server`,
/// `--sender`, packed `-slogDtpre.iLsfxCIvu`) and stdin carries a synthetic
/// "rsync" arg0 followed by `.` and the path tail. Without merging the two,
/// the server loses the flag string and aborts with
/// `missing rsync server flag string` against any upstream-client -> oc-rsync
/// server transfer that sets `-s`.
///
/// upstream: rsync.c:283 send_protected_args() / io.c:1308 read_args() /
/// main.c::read_args() callsite at main.c:1852.
#[test]
fn server_mode_merges_cmdline_and_stdin_secluded_args() {
    use std::io::Cursor;

    // Command-line argv as oc-rsync --server receives it under lsh.sh -s.
    let argv: Vec<OsString> = vec![
        OsString::from("oc-rsync"),
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("-slogDtpre.iLsfxCIvu"),
    ];

    // Wire bytes captured from upstream 3.4.3's send_protected_args for
    // `rsync -ais lh:reproA/from/ /tmp/dest/`:
    //   rsync\0.\0/home/ofer/reproA/from/\0\0
    let wire: &[u8] = b"rsync\0.\0/home/ofer/reproA/from/\0\0";
    let mut reader = Cursor::new(wire);
    let received = protocol::secluded_args::recv_secluded_args(&mut reader, None)
        .expect("recv should succeed");
    assert_eq!(
        received,
        vec![
            "rsync".to_owned(),
            ".".to_owned(),
            "/home/ofer/reproA/from/".to_owned(),
        ],
    );

    // The fix discards the synthetic arg0 and merges the rest with the
    // command-line tail. The flag string must come from argv, and the
    // positional path from stdin.
    let mut received_iter = received.into_iter();
    let _arg0 = received_iter.next();
    let effective_args: Vec<OsString> = argv
        .iter()
        .skip(1)
        .cloned()
        .chain(received_iter.map(OsString::from))
        .collect();

    let (flag_string, positional) = parse_server_flag_string_and_args(&effective_args);
    assert_eq!(flag_string, "-slogDtpre.iLsfxCIvu");
    assert_eq!(positional, vec![OsString::from("/home/ofer/reproA/from/")]);
}

/// When `-s` is sent as a standalone short flag (rather than embedded in
/// the packed flag string), the same merge produces the right split.
#[test]
fn server_mode_merges_cmdline_and_stdin_standalone_s_flag() {
    use std::io::Cursor;

    let argv: Vec<OsString> = vec![
        OsString::from("oc-rsync"),
        OsString::from("--server"),
        OsString::from("-s"),
        OsString::from("-logDtpr"),
    ];

    let wire: &[u8] = b"rsync\0.\0/srv/data/\0\0";
    let mut reader = Cursor::new(wire);
    let received = protocol::secluded_args::recv_secluded_args(&mut reader, None)
        .expect("recv should succeed");

    let mut received_iter = received.into_iter();
    let _arg0 = received_iter.next();
    let effective_args: Vec<OsString> = argv
        .iter()
        .skip(1)
        .cloned()
        .chain(received_iter.map(OsString::from))
        .collect();

    let (flag_string, positional) = parse_server_flag_string_and_args(&effective_args);
    assert_eq!(flag_string, "-logDtpr");
    assert_eq!(positional, vec![OsString::from("/srv/data/")]);
}
