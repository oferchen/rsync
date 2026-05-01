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

#[test]
fn list_only_enables_dry_run() {
    let result = parse_test_args(["--list-only", "src/", "dst/"]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.dry_run);
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
    assert!(parsed.show_version);
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
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
}

#[test]
fn human_readable_double_short_hh_is_combined() {
    let parsed = parse_test_args(["-hh", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
}

#[test]
fn human_readable_long_flag_is_enabled() {
    let parsed = parse_test_args(["--human-readable", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
}

#[test]
fn human_readable_explicit_level_two() {
    let parsed = parse_test_args(["--human-readable=2", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
}

#[test]
fn human_readable_explicit_level_zero() {
    let parsed = parse_test_args(["--human-readable=0", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Disabled));
}

#[test]
fn human_readable_no_flag_disables() {
    let parsed = parse_test_args(["--no-human-readable", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Disabled));
}

#[test]
fn human_readable_not_specified_is_none() {
    let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, None);
}

#[test]
fn human_readable_explicit_one_twice_stays_enabled() {
    let parsed = parse_test_args(["--human-readable=1", "--human-readable=1", "src/", "dst/"])
        .expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
}

#[test]
fn human_readable_two_bare_long_flags_is_combined() {
    let parsed =
        parse_test_args(["--human-readable", "--human-readable", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
}

#[test]
fn human_readable_two_separate_short_flags_is_combined() {
    let parsed = parse_test_args(["-h", "-h", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
}

// ── Embedded SSH arguments ──────────────────────────────────────────

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
