use super::*;
use core::client::HumanReadableMode;

/// Helper to parse arguments without the program name being auto-detected.
fn parse_test_args<I, S>(args: I) -> Result<ParsedArgs, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let iter = std::iter::once("rsync".to_string())
        .chain(args.into_iter().map(|s| s.as_ref().to_string()));
    parse_args(iter)
}

#[test]
fn delete_modes_are_mutually_exclusive_two_flags() {
    let result = parse_test_args(["-r", "--delete-before", "--delete-after", "src/", "dst/"]);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("mutually exclusive"));
}

#[test]
fn delete_modes_are_mutually_exclusive_three_flags() {
    let result = parse_test_args([
        "-r",
        "--delete-before",
        "--delete-during",
        "--delete-after",
        "src/",
        "dst/",
    ]);
    assert!(result.is_err());
}

#[test]
fn delete_requires_recursive_or_dirs() {
    let result = parse_test_args(["--delete", "src/", "dst/"]);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("--recursive"));
}

#[test]
fn delete_with_recursive_succeeds() {
    let result = parse_test_args(["-r", "--delete", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.delete_mode.is_enabled());
}

#[test]
fn delete_with_dirs_succeeds() {
    let result = parse_test_args(["-d", "--delete", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.delete_mode.is_enabled());
}

#[test]
fn delete_excluded_activates_delete_mode() {
    let result = parse_test_args(["-r", "--delete-excluded", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.delete_mode.is_enabled());
}

#[test]
fn max_delete_activates_delete_mode() {
    let result = parse_test_args(["-r", "--max-delete=10", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.delete_mode.is_enabled());
}

#[test]
fn perms_flag_only() {
    let result = parse_test_args(["--perms", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.perms, Some(true));
}

#[test]
fn no_perms_flag_only() {
    let result = parse_test_args(["--no-perms", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.perms, Some(false));
}

#[test]
fn neither_perms_flag() {
    let result = parse_test_args(["src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.perms, None);
}

#[test]
fn perms_then_no_perms_uses_last() {
    let result = parse_test_args(["--perms", "--no-perms", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.perms, Some(false));
}

#[test]
fn no_perms_then_perms_uses_last() {
    let result = parse_test_args(["--no-perms", "--perms", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.perms, Some(true));
}

#[test]
fn multiple_perms_alternations() {
    let result = parse_test_args([
        "--perms",
        "--no-perms",
        "--perms",
        "--no-perms",
        "--perms",
        "src/",
        "dst/",
    ]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.perms, Some(true));
}

#[test]
fn prune_empty_dirs_flag() {
    let result = parse_test_args(["--prune-empty-dirs", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.prune_empty_dirs, Some(true));
}

#[test]
fn no_prune_empty_dirs_flag() {
    let result = parse_test_args(["--no-prune-empty-dirs", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.prune_empty_dirs, Some(false));
}

#[test]
fn omit_link_times_flag() {
    let result = parse_test_args(["--omit-link-times", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.omit_link_times, Some(true));
}

// upstream: options.c:2366-2367 - `--list-only` does NOT set dry_run (only -n
// does). The receiver skips destination writes under list_only independently
// (run_client mode selection + TransferFlags::skip_dest_writes), so the two
// must stay decoupled or the server args wrongly pack the compact 'n' letter.
#[test]
fn list_only_does_not_enable_dry_run() {
    let result = parse_test_args(["--list-only", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(!parsed.dry_run, "list-only must not force dry_run");
    assert!(parsed.list_only);
}

#[test]
fn dry_run_without_list_only() {
    let result = parse_test_args(["--dry-run", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.dry_run);
    assert!(!parsed.list_only);
}

#[test]
fn archive_mode_sets_recursive() {
    let result = parse_test_args(["-a", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.recursive);
}

#[test]
fn archive_no_recursive_override() {
    let result = parse_test_args(["-a", "--no-recursive", "src/", "dst/"]);
    assert!(result.is_ok());
    let _parsed = result.unwrap();
}

#[test]
fn backup_dir_implies_backup() {
    let result = parse_test_args(["--backup-dir=/tmp/backup", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.backup);
}

#[test]
fn backup_suffix_implies_backup() {
    let result = parse_test_args(["--suffix=.bak", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.backup);
}

#[test]
fn usermap_twice_concatenates() {
    let result = parse_test_args(["--usermap=0:1000", "--usermap=100:2000", "src/", "dst/"]);
    let parsed = result.expect("multiple --usermap should succeed");
    assert_eq!(
        parsed.usermap,
        Some(std::ffi::OsString::from("0:1000,100:2000"))
    );
}

#[test]
fn groupmap_twice_concatenates() {
    let result = parse_test_args(["--groupmap=0:1000", "--groupmap=100:2000", "src/", "dst/"]);
    let parsed = result.expect("multiple --groupmap should succeed");
    assert_eq!(
        parsed.groupmap,
        Some(std::ffi::OsString::from("0:1000,100:2000"))
    );
}

#[test]
fn empty_rsh_uses_env() {
    let result = parse_test_args(["--rsh=", "src/", "dst/"]);
    assert!(result.is_ok());
    let _parsed = result.unwrap();
}

#[test]
fn verbose_flag() {
    let result = parse_test_args(["-v", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.verbosity > 0);
}

#[test]
fn multiple_verbose_flags() {
    let result = parse_test_args(["-vvv", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.verbosity >= 3);
}

#[test]
fn quiet_flag_reduces_verbosity() {
    let result = parse_test_args(["-q", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.verbosity, 0);
}

#[test]
fn compress_flag() {
    let result = parse_test_args(["-z", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.compress);
}

#[test]
fn no_compress_flag() {
    let result = parse_test_args(["--no-compress", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.no_compress);
}

#[test]
fn valid_port() {
    let result = parse_test_args(["--port=8873", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.daemon_port, Some(8873));
}

#[test]
fn port_max_value() {
    let result = parse_test_args(["--port=65535", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.daemon_port, Some(65535));
}

#[test]
fn help_flag() {
    let result = parse_test_args(["--help"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.show_help);
}

#[test]
fn version_flag() {
    let result = parse_test_args(["--version"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.show_version, 1);
}

#[test]
fn checksum_flag() {
    let result = parse_test_args(["-c", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.checksum, Some(true));
}

#[test]
fn times_flag() {
    let result = parse_test_args(["-t", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.times, Some(true));
}

#[test]
fn no_times_flag() {
    let result = parse_test_args(["--no-times", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.times, Some(false));
}

#[test]
fn aes_flag() {
    let result = parse_test_args(["--aes", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.prefer_aes_gcm, Some(true));
}

#[test]
fn aes_default_is_none() {
    let result = parse_test_args(["src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.prefer_aes_gcm, None);
}

#[test]
fn human_readable_single_short_h_is_enabled() {
    let parsed = parse_test_args(["-h", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::DecimalUnits));
}

#[test]
fn human_readable_double_short_hh_is_combined() {
    let parsed = parse_test_args(["-hh", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::BinaryUnits));
}

#[test]
fn human_readable_long_flag_is_enabled() {
    let parsed = parse_test_args(["--human-readable", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::DecimalUnits));
}

#[test]
fn human_readable_explicit_argument_is_rejected() {
    // upstream: options.c:616 declares -h/--human-readable as POPT_ARG_NONE, so
    // `--human-readable=N` errors with "option does not take an argument".
    assert!(parse_test_args(["--human-readable=2", "src/", "dst/"]).is_err());
    assert!(parse_test_args(["--human-readable=0", "src/", "dst/"]).is_err());
}

#[test]
fn human_readable_no_flag_is_raw_level_zero() {
    // upstream: options.c:617 resets human_readable to 0 (raw digits, width 11).
    let parsed = parse_test_args(["--no-human-readable", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Raw));
}

#[test]
fn human_readable_not_specified_is_none() {
    // None resolves to the upstream default level 1 (comma-grouped) downstream.
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, None);
}

#[test]
fn human_readable_no_h_then_h_prefers_h() {
    // upstream: overrides_with semantics - the later flag wins, so -h re-enables
    // suffix formatting after --no-h.
    let parsed = parse_test_args(["--no-h", "-h", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::DecimalUnits));
}

#[test]
fn human_readable_two_bare_long_flags_is_combined() {
    let parsed =
        parse_test_args(["--human-readable", "--human-readable", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::BinaryUnits));
}

#[test]
fn human_readable_two_separate_short_flags_is_combined() {
    let parsed = parse_test_args(["-h", "-h", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::BinaryUnits));
}

#[test]
fn ssh_cipher_parses_comma_separated_list() {
    let parsed = parse_test_args([
        "--ssh-cipher",
        "aes256-gcm,chacha20-poly1305",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert_eq!(
        parsed.ssh_cipher,
        vec!["aes256-gcm".to_string(), "chacha20-poly1305".to_string()]
    );
}

#[test]
fn ssh_cipher_defaults_to_empty() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert!(parsed.ssh_cipher.is_empty());
}

#[test]
fn ssh_connect_timeout_parses_seconds() {
    let parsed = parse_test_args(["--ssh-connect-timeout", "30", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.ssh_connect_timeout, Some(30));
}

#[test]
fn ssh_connect_timeout_defaults_to_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert_eq!(parsed.ssh_connect_timeout, None);
}

#[test]
fn ssh_keepalive_parses_seconds() {
    let parsed = parse_test_args(["--ssh-keepalive", "60", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.ssh_keepalive, Some(60));
}

#[test]
fn ssh_keepalive_zero_disables() {
    let parsed = parse_test_args(["--ssh-keepalive", "0", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.ssh_keepalive, Some(0));
}

#[test]
fn ssh_identity_single_file() {
    let parsed = parse_test_args([
        "--ssh-identity",
        "/home/user/.ssh/id_ed25519",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert_eq!(
        parsed.ssh_identity,
        vec![std::path::PathBuf::from("/home/user/.ssh/id_ed25519")]
    );
}

#[test]
fn ssh_identity_multiple_files() {
    let parsed = parse_test_args([
        "--ssh-identity",
        "/home/user/.ssh/id_ed25519",
        "--ssh-identity",
        "/home/user/.ssh/id_rsa",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert_eq!(parsed.ssh_identity.len(), 2);
}

#[test]
fn ssh_no_agent_flag() {
    let parsed = parse_test_args(["--ssh-no-agent", "src/", "dst/"]).expect("parse");
    assert!(parsed.ssh_no_agent);
}

#[test]
fn ssh_no_agent_defaults_to_false() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert!(!parsed.ssh_no_agent);
}

#[test]
fn ssh_strict_host_key_checking_yes() {
    let parsed =
        parse_test_args(["--ssh-strict-host-key-checking", "yes", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.ssh_strict_host_key_checking, Some("yes".to_string()));
}

#[test]
fn ssh_strict_host_key_checking_defaults_to_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert_eq!(parsed.ssh_strict_host_key_checking, None);
}

#[test]
fn ssh_ipv6_flag() {
    let parsed = parse_test_args(["--ssh-ipv6", "src/", "dst/"]).expect("parse");
    assert!(parsed.ssh_ipv6);
}

#[test]
fn ssh_ipv6_defaults_to_false() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert!(!parsed.ssh_ipv6);
}

#[test]
fn ssh_port_parses_number() {
    let parsed = parse_test_args(["--ssh-port", "2222", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.ssh_port, Some(2222));
}

#[test]
fn ssh_port_defaults_to_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert_eq!(parsed.ssh_port, None);
}

#[test]
fn jump_host_single_value() {
    let parsed =
        parse_test_args(["--jump-host", "bastion.example.com", "src/", "dst/"]).expect("parse");
    assert_eq!(
        parsed.jump_host.as_deref(),
        Some(std::ffi::OsStr::new("bastion.example.com"))
    );
}

#[test]
fn jump_host_multi_hop_value() {
    let parsed = parse_test_args([
        "--jump-host",
        "alice@a.example.com,bob@b.example.com",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert_eq!(
        parsed.jump_host.as_deref(),
        Some(std::ffi::OsStr::new(
            "alice@a.example.com,bob@b.example.com"
        ))
    );
}

#[test]
fn jump_host_with_port() {
    let parsed = parse_test_args([
        "--jump-host",
        "user@bastion.example.com:2200",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert_eq!(
        parsed.jump_host.as_deref(),
        Some(std::ffi::OsStr::new("user@bastion.example.com:2200"))
    );
}

#[test]
fn jump_host_empty_value_filtered() {
    let parsed = parse_test_args(["--jump-host", "", "src/", "dst/"]).expect("parse");
    assert!(parsed.jump_host.is_none());
}

#[test]
fn jump_host_defaults_to_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert!(parsed.jump_host.is_none());
}

#[test]
fn zero_copy_default_is_auto() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert_eq!(parsed.zero_copy_policy, fast_io::ZeroCopyPolicy::Auto);
}

#[test]
fn zero_copy_flag_sets_enabled() {
    let parsed = parse_test_args(["--zero-copy", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.zero_copy_policy, fast_io::ZeroCopyPolicy::Enabled);
}

#[test]
fn no_zero_copy_flag_sets_disabled() {
    let parsed = parse_test_args(["--no-zero-copy", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.zero_copy_policy, fast_io::ZeroCopyPolicy::Disabled);
}

#[test]
fn parallel_delta_scan_default_is_false() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert!(!parsed.parallel_delta_scan);
}

#[test]
fn parallel_delta_scan_flag_sets_true() {
    let parsed = parse_test_args(["--parallel-delta-scan", "src/", "dst/"]).expect("parse");
    assert!(parsed.parallel_delta_scan);
}

#[test]
fn zero_copy_then_no_zero_copy_last_wins() {
    let parsed = parse_test_args(["--zero-copy", "--no-zero-copy", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.zero_copy_policy, fast_io::ZeroCopyPolicy::Disabled);
}

#[test]
fn no_zero_copy_then_zero_copy_last_wins() {
    let parsed = parse_test_args(["--no-zero-copy", "--zero-copy", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.zero_copy_policy, fast_io::ZeroCopyPolicy::Enabled);
}

#[test]
fn zero_copy_is_independent_from_io_uring() {
    let parsed = parse_test_args(["--no-zero-copy", "--io-uring", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.zero_copy_policy, fast_io::ZeroCopyPolicy::Disabled);
    assert_eq!(parsed.io_uring_policy, fast_io::IoUringPolicy::Enabled);
}

#[test]
fn simd_defaults_to_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert_eq!(parsed.simd_override, None);
}

#[test]
fn simd_accepts_each_canonical_level() {
    for (input, expected) in [
        ("auto", checksums::SimdLevel::Auto),
        ("avx512", checksums::SimdLevel::Avx512),
        ("avx2", checksums::SimdLevel::Avx2),
        ("sse4", checksums::SimdLevel::Sse4),
        ("neon", checksums::SimdLevel::Neon),
        ("none", checksums::SimdLevel::None),
    ] {
        let arg = format!("--simd={input}");
        let parsed = parse_test_args([arg.as_str(), "src/", "dst/"])
            .unwrap_or_else(|err| panic!("parse failed for {input}: {err}"));
        assert_eq!(
            parsed.simd_override,
            Some(expected),
            "level {input} parsed incorrectly",
        );
    }
}

#[test]
fn simd_accepts_aliases() {
    let parsed = parse_test_args(["--simd=AVX-512", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.simd_override, Some(checksums::SimdLevel::Avx512));

    let parsed = parse_test_args(["--simd=sse4.1", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.simd_override, Some(checksums::SimdLevel::Sse4));

    let parsed = parse_test_args(["--simd=scalar", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.simd_override, Some(checksums::SimdLevel::None));
}

#[test]
fn simd_rejects_unknown_levels() {
    let result = parse_test_args(["--simd=avx1024", "src/", "dst/"]);
    let err = result.expect_err("unknown level should fail");
    assert!(err.to_string().contains("--simd"));
    assert!(err.to_string().contains("avx1024"));
}

#[test]
fn spill_dir_flag_default_is_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert!(parsed.spill_dir.is_none());
}

#[test]
fn spill_threshold_bytes_flag_default_is_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert!(parsed.spill_threshold_bytes.is_none());
}

#[test]
fn spill_dir_flag_parses_into_pathbuf() {
    let parsed =
        parse_test_args(["--spill-dir", "/tmp/oc-rsync-spill", "src/", "dst/"]).expect("parse");
    assert_eq!(
        parsed.spill_dir.as_deref(),
        Some(std::path::Path::new("/tmp/oc-rsync-spill"))
    );
}

#[test]
fn spill_threshold_bytes_flag_parses_plain_integer() {
    let parsed =
        parse_test_args(["--spill-threshold-bytes", "1048576", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.spill_threshold_bytes, Some(1_048_576));
}

#[test]
fn spill_threshold_bytes_flag_parses_kilo_suffix() {
    let parsed =
        parse_test_args(["--spill-threshold-bytes", "64K", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.spill_threshold_bytes, Some(64 * 1024));
}

#[test]
fn spill_threshold_bytes_flag_parses_mega_suffix_case_insensitive() {
    let parsed = parse_test_args(["--spill-threshold-bytes", "8m", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.spill_threshold_bytes, Some(8 * 1024 * 1024));
}

#[test]
fn spill_threshold_bytes_flag_parses_giga_suffix() {
    let parsed = parse_test_args(["--spill-threshold-bytes", "2G", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.spill_threshold_bytes, Some(2 * 1024 * 1024 * 1024));
}

#[test]
fn spill_threshold_bytes_rejects_zero() {
    let err = parse_test_args(["--spill-threshold-bytes", "0", "src/", "dst/"])
        .expect_err("zero should be rejected");
    assert!(err.to_string().contains("--spill-threshold-bytes"));
    assert!(err.to_string().contains("greater than zero"));
}

#[test]
fn spill_threshold_bytes_rejects_unknown_suffix() {
    let err = parse_test_args(["--spill-threshold-bytes", "10Q", "src/", "dst/"])
        .expect_err("unknown suffix should be rejected");
    assert!(err.to_string().contains("--spill-threshold-bytes"));
    assert!(err.to_string().contains("K/M/G/T/P/E suffix"));
}

#[test]
fn spill_threshold_bytes_rejects_non_numeric() {
    let err = parse_test_args(["--spill-threshold-bytes", "abc", "src/", "dst/"])
        .expect_err("non-numeric should be rejected");
    assert!(err.to_string().contains("--spill-threshold-bytes"));
}

#[test]
fn spill_threshold_bytes_rejects_empty_value() {
    let err = parse_test_args(["--spill-threshold-bytes", "", "src/", "dst/"])
        .expect_err("empty should be rejected");
    assert!(err.to_string().contains("--spill-threshold-bytes"));
    assert!(err.to_string().contains("must not be empty"));
}

#[test]
fn spill_dir_and_threshold_can_be_combined() {
    let parsed = parse_test_args([
        "--spill-dir",
        "/var/spool/oc-rsync",
        "--spill-threshold-bytes",
        "128M",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert_eq!(
        parsed.spill_dir.as_deref(),
        Some(std::path::Path::new("/var/spool/oc-rsync"))
    );
    assert_eq!(parsed.spill_threshold_bytes, Some(128 * 1024 * 1024));
}

#[test]
fn no_spill_flag_default_is_false() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert!(!parsed.no_spill);
}

#[test]
fn no_spill_flag_sets_true() {
    let parsed = parse_test_args(["--no-spill", "src/", "dst/"]).expect("parse");
    assert!(parsed.no_spill);
}

#[test]
fn no_spill_combines_with_spill_dir_and_threshold() {
    let parsed = parse_test_args([
        "--no-spill",
        "--spill-dir",
        "/tmp/spill",
        "--spill-threshold-bytes",
        "64K",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert!(parsed.no_spill);
    assert_eq!(
        parsed.spill_dir.as_deref(),
        Some(std::path::Path::new("/tmp/spill"))
    );
    assert_eq!(parsed.spill_threshold_bytes, Some(64 * 1024));
}

/// `--reflink` defaults to `auto`, which surfaces as
/// [`fast_io::CowPolicy::Auto`] so the existing default reflink path is
/// preserved when neither the binary nor the tri-state form is given.
#[test]
fn reflink_default_is_auto() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert_eq!(parsed.cow_policy, fast_io::CowPolicy::Auto);
}

#[test]
fn reflink_auto_parses() {
    let parsed = parse_test_args(["--reflink=auto", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.cow_policy, fast_io::CowPolicy::Auto);
}

#[test]
fn reflink_always_parses_to_required() {
    let parsed = parse_test_args(["--reflink=always", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.cow_policy, fast_io::CowPolicy::Required);
}

#[test]
fn reflink_never_parses_to_disabled() {
    let parsed = parse_test_args(["--reflink=never", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.cow_policy, fast_io::CowPolicy::Disabled);
}

/// Any value outside the tri-state vocabulary must fail at parse time
/// with the canonical "expected one of ..." error message.
#[test]
fn reflink_bogus_value_errors_at_parse_time() {
    let err = parse_test_args(["--reflink=bogus", "src/", "dst/"]).expect_err("parse must fail");
    let rendered = err.to_string();
    assert!(
        rendered.contains("--reflink"),
        "error must mention the flag: {rendered}"
    );
    assert!(
        rendered.contains("auto") && rendered.contains("always") && rendered.contains("never"),
        "error must list valid values: {rendered}"
    );
}

/// `--reflink` after `--cow`/`--no-cow` wins, matching upstream
/// left-to-right option processing.
#[test]
fn reflink_after_binary_form_overrides() {
    let parsed = parse_test_args(["--no-cow", "--reflink=always", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.cow_policy, fast_io::CowPolicy::Required);
}

/// `--no-cow` after `--reflink=always` wins.
#[test]
fn binary_form_after_reflink_overrides() {
    let parsed = parse_test_args(["--reflink=always", "--no-cow", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.cow_policy, fast_io::CowPolicy::Disabled);
}

/// `--reflink=never` is wire-equivalent to `--no-cow` once parsed.
#[test]
fn reflink_never_matches_no_cow() {
    let from_reflink =
        parse_test_args(["--reflink=never", "src/", "dst/"]).expect("parse reflink form");
    let from_binary = parse_test_args(["--no-cow", "src/", "dst/"]).expect("parse binary form");
    assert_eq!(from_reflink.cow_policy, from_binary.cow_policy);
}

/// upstream: `testsuite/exclude.test:173` uses `-f:C` to inject the
/// dir-merge .cvsignore rule via the packed short-arg form. Verify the
/// pre-Clap expansion routes `:C` through to the filter list as the rule.
#[test]
fn filter_short_arg_packed_colon_rule() {
    let parsed = parse_test_args(["-f:C", "src/", "dst/"]).expect("parse -f:C");
    assert_eq!(parsed.filters, vec![std::ffi::OsString::from(":C")]);
}

/// `-f-C` is the packed spelling for `-f` with value `-C` (cvs-style
/// exclude). It must round-trip through the parser without being mistaken
/// for an unknown option or the `-C` short.
#[test]
fn filter_short_arg_packed_dash_rule() {
    let parsed = parse_test_args(["-f-C", "src/", "dst/"]).expect("parse -f-C");
    assert_eq!(parsed.filters, vec![std::ffi::OsString::from("-C")]);
    assert!(!parsed.cvs_exclude);
}

/// Spaced form `-f :C` must produce the same filter rule as the packed form.
#[test]
fn filter_short_arg_spaced_colon_rule() {
    let parsed = parse_test_args(["-f", ":C", "src/", "dst/"]).expect("parse -f :C");
    assert_eq!(parsed.filters, vec![std::ffi::OsString::from(":C")]);
}

/// Combined invocation from `testsuite/exclude.test`: `-f:C -f-C` injects
/// the two cvs-exclude filter rules in order.
#[test]
fn filter_short_arg_packed_pair_matches_exclude_test() {
    let parsed = parse_test_args(["-f:C", "-f-C", "src/", "dst/"]).expect("parse -f:C -f-C");
    assert_eq!(
        parsed.filters,
        vec![
            std::ffi::OsString::from(":C"),
            std::ffi::OsString::from("-C"),
        ]
    );
}

/// `-f:C` may also appear inside a flag cluster (`-avvf:C`). The leading
/// flags are split out and the trailing value option keeps its packed rule.
#[test]
fn filter_short_arg_packed_inside_flag_cluster() {
    let parsed = parse_test_args(["-avvf:C", "src/", "dst/"]).expect("parse -avvf:C");
    assert_eq!(parsed.filters, vec![std::ffi::OsString::from(":C")]);
    assert!(parsed.archive);
}

#[test]
fn checksum_threads_defaults_to_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse without flag");
    assert_eq!(parsed.checksum_threads, None);
}

#[test]
fn checksum_threads_accepts_auto_zero_one_and_n() {
    use crate::frontend::arguments::ChecksumThreadsSetting;
    for (arg, expected) in [
        ("auto", ChecksumThreadsSetting::Auto),
        ("AUTO", ChecksumThreadsSetting::Auto),
        ("0", ChecksumThreadsSetting::Auto),
        ("1", ChecksumThreadsSetting::Sequential),
        ("8", ChecksumThreadsSetting::Capped(8)),
    ] {
        let parsed = parse_test_args([&format!("--checksum-threads={arg}"), "src/", "dst/"])
            .unwrap_or_else(|e| panic!("parse --checksum-threads={arg}: {e}"));
        assert_eq!(parsed.checksum_threads, Some(expected), "value {arg}");
    }
}

#[test]
fn checksum_threads_rejects_invalid_and_out_of_range() {
    for arg in ["nope", "-1", "2048", ""] {
        let result = parse_test_args([&format!("--checksum-threads={arg}"), "src/", "dst/"]);
        assert!(result.is_err(), "value {arg:?} must be rejected");
    }
}

/// Builds `count` repetitions of a single alt-dest option plus operands.
fn alt_dest_args(flag: &str, count: usize) -> Vec<String> {
    let mut args: Vec<String> = (0..count)
        .map(|index| format!("{flag}=dir{index}"))
        .collect();
    args.push("src/".to_string());
    args.push("dst/".to_string());
    args
}

/// upstream: options.c:1749-1754 accepts up to `MAX_BASIS_DIRS` (rsync.h:196)
/// alt-dest args; the boundary itself must not be rejected.
#[test]
fn alt_dest_limit_accepts_twenty() {
    let result = parse_test_args(alt_dest_args("--link-dest", 20));
    assert!(result.is_ok(), "20 alt-dest dirs must be accepted");
}

/// upstream: options.c:1752-1753 rejects the 21st alt-dest arg with the verbatim
/// `ERROR: at most 20 <opt> args may be specified` message and RERR_SYNTAX. The
/// exact wording matters because it is observable output a drop-in must mirror.
#[test]
fn alt_dest_limit_rejects_twenty_one() {
    let result = parse_test_args(alt_dest_args("--link-dest", 21));
    let err = result.expect_err("21 alt-dest dirs must be rejected");
    assert_eq!(err.kind(), clap::error::ErrorKind::TooManyValues);
    assert!(
        err.to_string()
            .contains("ERROR: at most 20 --link-dest args may be specified"),
        "unexpected message: {err}"
    );
}

/// The cap counts the shared `basis_dir[]` array, so it fires for whichever of
/// the three alt-dest option types is in use and names that option verbatim
/// (upstream: `alt_dest_opt(0)`). Every type must be capped identically.
#[test]
fn alt_dest_limit_enforced_for_each_type() {
    for flag in ["--compare-dest", "--copy-dest", "--link-dest"] {
        assert!(
            parse_test_args(alt_dest_args(flag, 20)).is_ok(),
            "20 {flag} dirs must be accepted"
        );
        let err = parse_test_args(alt_dest_args(flag, 21))
            .expect_err(&format!("21 {flag} dirs must be rejected"));
        assert!(
            err.to_string()
                .contains(&format!("ERROR: at most 20 {flag} args may be specified")),
            "unexpected message for {flag}: {err}"
        );
    }
}

/// The `basis_dir[]` array is shared, but upstream forbids mixing the three
/// alt-dest types (options.c:1741-1745), a rule oc mirrors with
/// `conflicts_with_all`. So a "combined" total spanning types (here 10
/// `--compare-dest` + 11 `--link-dest`) can never reach the count check - it is
/// rejected first by the conflict rule. This proves only one type ever populates
/// the shared array, which is why the cap is enforced per type in effect.
#[test]
fn alt_dest_types_cannot_be_mixed() {
    let mut args: Vec<String> = (0..10).map(|i| format!("--compare-dest=c{i}")).collect();
    args.extend((0..11).map(|i| format!("--link-dest=l{i}")));
    args.push("src/".to_string());
    args.push("dst/".to_string());

    let err = parse_test_args(args).expect_err("mixed alt-dest types must be rejected");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    assert!(
        err.to_string().contains("cannot be used with"),
        "expected a conflict diagnostic, got: {err}"
    );
}
