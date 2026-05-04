//! Tests for remote invocation builder and transfer role detection.

use std::ffi::{OsStr, OsString};

use compress::algorithm::CompressionAlgorithm;
use transfer::setup::build_capability_string;

use super::builder::RemoteInvocationBuilder;
use super::transfer_role::{determine_transfer_role, operand_is_remote};
use super::{RemoteOperands, RemoteRole, TransferSpec};
use crate::client::config::{ClientConfig, IconvSetting, TransferTimeout};

#[test]
fn builds_receiver_invocation_with_sender_flag() {
    // Pull: local is receiver -> remote needs --sender (upstream options.c:2598)
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");

    assert_eq!(args[0], "rsync");
    assert_eq!(args[1], "--server");
    assert_eq!(args[2], "--sender");
    let flags = args[3].to_string_lossy();
    assert!(flags.starts_with('-'), "flags should start with -: {flags}");
    let expected_caps = build_capability_string(true);
    assert_eq!(args[4], *expected_caps);
    assert_eq!(args[5], ".");
    assert_eq!(args[6], "/remote/path");
}

#[test]
fn builds_sender_invocation_no_sender_flag() {
    // Push: local is sender -> remote is receiver, no --sender flag
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    assert_eq!(args[0], "rsync");
    assert_eq!(args[1], "--server");
    // No --sender flag for push - flags come next
    let flags = args[2].to_string_lossy();
    assert!(flags.starts_with('-'), "flags should start with -: {flags}");
    let expected_caps = build_capability_string(true);
    assert_eq!(args[3], *expected_caps);
    assert_eq!(args[4], ".");
    assert_eq!(args[5], "/remote/path");
}

#[test]
fn ssh_sender_advertises_inc_recurse_capability_by_default() {
    // Default: SSH push transfers advertise the 'i' capability bit, mirroring
    // upstream rsync's `allow_inc_recurse = 1` initialization. Tracker #1862.
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    let caps = args
        .iter()
        .find(|a| a.to_string_lossy().starts_with("-e."))
        .expect("capability string present");
    let caps_str = caps.to_string_lossy();
    assert!(
        caps_str.contains('i'),
        "default sender capability string must advertise 'i': {caps_str}"
    );
    assert_eq!(*caps, *build_capability_string(true));
}

#[test]
fn ssh_sender_omits_inc_recurse_when_no_inc_recursive_set() {
    // `--no-inc-recursive` clears `allow_inc_recurse`; the capability bit
    // is suppressed in both transfer directions. Tracker #1862.
    let config = ClientConfig::builder().inc_recursive_send(false).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    let caps = args
        .iter()
        .find(|a| a.to_string_lossy().starts_with("-e."))
        .expect("capability string present");
    let caps_str = caps.to_string_lossy();
    assert!(
        !caps_str.contains('i'),
        "--no-inc-recursive must suppress 'i' on sender capability: {caps_str}"
    );
    assert_eq!(*caps, *build_capability_string(false));
}

#[test]
fn ssh_receiver_advertises_inc_recurse_capability_by_default() {
    // Pull transfers (local is receiver) also advertise 'i' by default so the
    // remote sender's `set_allow_inc_recurse()` keeps `allow_inc_recurse = 1`.
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");

    let caps = args
        .iter()
        .find(|a| a.to_string_lossy().starts_with("-e."))
        .expect("capability string present");
    let caps_str = caps.to_string_lossy();
    assert!(
        caps_str.contains('i'),
        "default receiver capability string must advertise 'i': {caps_str}"
    );
}

#[test]
fn ssh_receiver_omits_inc_recurse_when_no_inc_recursive_set() {
    // `--no-inc-recursive` applies to both directions, matching upstream's
    // single `allow_inc_recurse` global.
    let config = ClientConfig::builder().inc_recursive_send(false).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");

    let caps = args
        .iter()
        .find(|a| a.to_string_lossy().starts_with("-e."))
        .expect("capability string present");
    let caps_str = caps.to_string_lossy();
    assert!(
        !caps_str.contains('i'),
        "--no-inc-recursive must suppress 'i' on receiver capability: {caps_str}"
    );
}

#[test]
fn includes_recursive_flag_when_enabled() {
    let config = ClientConfig::builder().recursive(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    // Sender (push): rsync --server -flags . /path - flags at index 2
    let flags = args[2].to_string_lossy();
    assert!(flags.contains('r'), "expected 'r' in flags: {flags}");
}

#[test]
fn includes_multiple_preservation_flags() {
    let config = ClientConfig::builder()
        .times(true)
        .permissions(true)
        .owner(true)
        .group(true)
        .build();

    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    // Sender (push): rsync --server -flags . /path - flags at index 2
    let flags = args[2].to_string_lossy();
    assert!(flags.contains('t'), "expected 't' in flags: {flags}");
    assert!(flags.contains('p'), "expected 'p' in flags: {flags}");
    assert!(flags.contains('o'), "expected 'o' in flags: {flags}");
    assert!(flags.contains('g'), "expected 'g' in flags: {flags}");
}

#[test]
fn includes_compress_flag() {
    let config = ClientConfig::builder().compress(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    // Sender (push): rsync --server -flags . /path - flags at index 2
    let flags = args[2].to_string_lossy();
    assert!(flags.contains('z'), "expected 'z' in flags: {flags}");
}

#[test]
fn includes_log_format_for_itemize() {
    // upstream: options.c:2750-2762 - itemize is sent as --log-format=%i
    let config = ClientConfig::builder().itemize_changes(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let args_str: Vec<_> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        args_str.contains(&"--log-format=%i".to_string()),
        "expected --log-format=%i in args: {args_str:?}"
    );
    // Must NOT appear as a compact flag
    let flags = args[2].to_string_lossy();
    assert!(
        !flags.contains(".i"),
        "itemize should not be a compact flag: {flags}"
    );
}

#[test]
fn omits_log_format_when_itemize_disabled() {
    let config = ClientConfig::builder().itemize_changes(false).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let args_str: Vec<_> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        !args_str.contains(&"--log-format=%i".to_string()),
        "unexpected --log-format=%i in args: {args_str:?}"
    );
}

#[test]
fn detects_push_when_destination_remote() {
    let sources = vec![OsString::from("local.txt")];
    let destination = OsString::from("user@host:/remote.txt");

    let result = determine_transfer_role(&sources, &destination).unwrap();

    assert_eq!(result.role(), RemoteRole::Sender);
    match result {
        TransferSpec::Push {
            local_sources,
            remote_dest,
        } => {
            assert_eq!(local_sources, vec!["local.txt"]);
            assert_eq!(remote_dest, "user@host:/remote.txt");
        }
        _ => panic!("Expected Push transfer"),
    }
}

#[test]
fn detects_pull_when_source_remote() {
    let sources = vec![OsString::from("user@host:/remote.txt")];
    let destination = OsString::from("local.txt");

    let result = determine_transfer_role(&sources, &destination).unwrap();

    assert_eq!(result.role(), RemoteRole::Receiver);
    match result {
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            assert_eq!(local_dest, "local.txt");
            assert_eq!(
                remote_sources,
                RemoteOperands::Single("user@host:/remote.txt".to_owned())
            );
        }
        _ => panic!("Expected Pull transfer"),
    }
}

#[test]
fn detects_push_with_multiple_sources() {
    let sources = vec![OsString::from("file1.txt"), OsString::from("file2.txt")];
    let destination = OsString::from("host:/dest/");

    let result = determine_transfer_role(&sources, &destination).unwrap();

    assert_eq!(result.role(), RemoteRole::Sender);
    match result {
        TransferSpec::Push {
            local_sources,
            remote_dest,
        } => {
            assert_eq!(local_sources, vec!["file1.txt", "file2.txt"]);
            assert_eq!(remote_dest, "host:/dest/");
        }
        _ => panic!("Expected Push transfer"),
    }
}

#[test]
fn detects_proxy_when_both_remote() {
    let sources = vec![OsString::from("host1:/file")];
    let destination = OsString::from("host2:/file");

    let result = determine_transfer_role(&sources, &destination).unwrap();
    assert_eq!(result.role(), RemoteRole::Proxy);
    match result {
        TransferSpec::Proxy {
            remote_sources,
            remote_dest,
        } => {
            assert_eq!(
                remote_sources,
                RemoteOperands::Single("host1:/file".to_owned())
            );
            assert_eq!(remote_dest, "host2:/file");
        }
        _ => panic!("Expected Proxy transfer"),
    }
}

#[test]
fn rejects_neither_remote() {
    let sources = vec![OsString::from("local1.txt")];
    let destination = OsString::from("local2.txt");

    let result = determine_transfer_role(&sources, &destination);
    assert!(result.is_err());
}

#[test]
fn rejects_mixed_remote_and_local_sources() {
    let sources = vec![
        OsString::from("local.txt"),
        OsString::from("host:/remote.txt"),
    ];
    let destination = OsString::from("dest/");

    let result = determine_transfer_role(&sources, &destination);
    assert!(result.is_err());
}

#[test]
fn accepts_multiple_remote_sources_same_host() {
    let sources = vec![OsString::from("host:/file1"), OsString::from("host:/file2")];
    let destination = OsString::from("dest/");

    let result = determine_transfer_role(&sources, &destination).unwrap();
    assert_eq!(result.role(), RemoteRole::Receiver);
    match result {
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            assert_eq!(local_dest, "dest/");
            assert_eq!(
                remote_sources,
                RemoteOperands::Multiple(vec!["host:/file1".to_owned(), "host:/file2".to_owned()])
            );
        }
        _ => panic!("Expected Pull transfer"),
    }
}

#[test]
fn rejects_multiple_remote_sources_different_hosts() {
    let sources = vec![
        OsString::from("host1:/file1"),
        OsString::from("host2:/file2"),
    ];
    let destination = OsString::from("dest/");

    let result = determine_transfer_role(&sources, &destination);
    assert!(result.is_err());
}

#[test]
fn includes_ignore_errors_flag_when_set() {
    let config = ClientConfig::builder().ignore_errors(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    // --ignore-errors should appear after --server
    assert!(
        args.iter().any(|a| a == "--ignore-errors"),
        "expected --ignore-errors in args: {args:?}"
    );
}

#[test]
fn omits_ignore_errors_flag_when_not_set() {
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    // --ignore-errors should not appear
    assert!(
        !args.iter().any(|a| a == "--ignore-errors"),
        "unexpected --ignore-errors in args: {args:?}"
    );
}

#[test]
fn includes_fsync_flag_when_set() {
    let config = ClientConfig::builder().fsync(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/path");

    // --fsync should appear after --server
    assert!(
        args.iter().any(|a| a == "--fsync"),
        "expected --fsync in args: {args:?}"
    );
}

#[test]
fn omits_fsync_flag_when_not_set() {
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/path");

    // --fsync should not appear
    assert!(
        !args.iter().any(|a| a == "--fsync"),
        "unexpected --fsync in args: {args:?}"
    );
}

/// Allowlist of long-form argument prefixes that upstream rsync 3.x recognises
/// in `--server` mode.  Any long flag emitted by `RemoteInvocationBuilder`
/// whose prefix is NOT on this list would break interop with stock rsync.
/// Arguments with `=value` suffixes are matched by prefix (e.g. `--timeout=30`
/// matches the `--timeout` prefix).
const UPSTREAM_SERVER_LONG_ARGS: &[&str] = &[
    "--server",
    "--sender",
    "--ignore-errors",
    "--fsync",
    "--delete-before",
    "--delete-during",
    "--delete-after",
    "--delete-delay",
    "--delete-excluded",
    "--force",
    "--max-delete",
    "--max-size",
    "--min-size",
    "--modify-window",
    "--compress-level",
    "--compress-choice",
    "--checksum-choice",
    "--block-size",
    "--timeout",
    "--bwlimit",
    "--partial-dir",
    "--temp-dir",
    "--inplace",
    "--append",
    "--append-verify",
    "--copy-unsafe-links",
    "--safe-links",
    "--munge-links",
    "--numeric-ids",
    "--size-only",
    "--ignore-times",
    "--ignore-existing",
    "--existing",
    "--remove-source-files",
    "--no-implied-dirs",
    "--fake-super",
    "--omit-dir-times",
    "--omit-link-times",
    "--delay-updates",
    "--backup",
    "--backup-dir",
    "--suffix",
    "--compare-dest",
    "--copy-dest",
    "--link-dest",
    "--copy-devices",
    "--write-devices",
    "--open-noatime",
    "--preallocate",
];

/// Returns whether a long-form argument matches one of the upstream allowlist
/// entries, accounting for `=value` suffixes.
fn is_upstream_compatible_long_arg(arg: &str) -> bool {
    UPSTREAM_SERVER_LONG_ARGS
        .iter()
        .any(|&allowed| arg == allowed || arg.starts_with(&format!("{allowed}=")))
}

/// Validate that every argument sent to the remote server is compatible
/// with upstream rsync's `--server` mode.  This catches regressions where
/// an oc-rsync-only flag accidentally leaks into the remote invocation.
#[test]
fn remote_invocation_only_sends_upstream_compatible_args() {
    // Build a config with every oc-rsync extension enabled so we can
    // verify none of them leak into the remote argument vector.
    let config = ClientConfig::builder()
        .fsync(true)
        .ignore_errors(true)
        .recursive(true)
        .links(true)
        .owner(true)
        .group(true)
        .times(true)
        .permissions(true)
        .compress(true)
        .checksum(true)
        .sparse(true)
        .build();

    for role in [RemoteRole::Sender, RemoteRole::Receiver] {
        let builder = RemoteInvocationBuilder::new(&config, role);
        let args = builder.build("/path");

        for arg in &args {
            let s = arg.to_string_lossy();

            // Skip the program name, the "." placeholder, and remote paths
            if s == "rsync" || s == "." || !s.starts_with('-') {
                continue;
            }

            // Compact flag strings (single dash, not "--") are upstream-compatible
            // by construction - they use the same single-char flags as upstream.
            if s.starts_with('-') && !s.starts_with("--") {
                continue;
            }

            // Long-form args must be on the upstream allowlist
            assert!(
                is_upstream_compatible_long_arg(&s),
                "remote invocation contains non-upstream long arg {s:?} \
                 (role={role:?}, full args={args:?}). \
                 If this is intentional, add it to UPSTREAM_SERVER_LONG_ARGS \
                 after verifying upstream rsync accepts it."
            );
        }
    }
}

#[test]
fn includes_delete_before_long_arg() {
    let config = ClientConfig::builder().delete_before(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--delete-before"),
        "expected --delete-before in args: {args:?}"
    );
}

#[test]
fn includes_delete_after_long_arg() {
    let config = ClientConfig::builder().delete_after(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--delete-after"),
        "expected --delete-after in args: {args:?}"
    );
}

#[test]
fn includes_delete_excluded_long_arg() {
    let config = ClientConfig::builder().delete_excluded(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--delete-excluded"),
        "expected --delete-excluded in args: {args:?}"
    );
}

#[test]
fn includes_timeout_long_arg() {
    use std::num::NonZeroU64;
    let config = ClientConfig::builder()
        .timeout(TransferTimeout::Seconds(NonZeroU64::new(30).unwrap()))
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--timeout=30"),
        "expected --timeout=30 in args: {args:?}"
    );
}

#[test]
fn includes_bwlimit_long_arg() {
    use crate::client::config::BandwidthLimit;
    use std::num::NonZeroU64;
    let config = ClientConfig::builder()
        .bandwidth_limit(Some(BandwidthLimit::from_bytes_per_second(
            NonZeroU64::new(1024).unwrap(),
        )))
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter()
            .any(|a| a.to_string_lossy().starts_with("--bwlimit=")),
        "expected --bwlimit=... in args: {args:?}"
    );
}

#[test]
fn includes_inplace_long_arg() {
    let config = ClientConfig::builder().inplace(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--inplace"),
        "expected --inplace in args: {args:?}"
    );
}

#[test]
fn includes_partial_dir_long_arg() {
    let config = ClientConfig::builder()
        .partial_directory(Some(".rsync-partial"))
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter()
            .any(|a| a.to_string_lossy() == "--partial-dir=.rsync-partial"),
        "expected --partial-dir=.rsync-partial in args: {args:?}"
    );
}

#[test]
fn includes_checksum_choice_long_arg() {
    use crate::client::config::StrongChecksumChoice;
    let choice = StrongChecksumChoice::parse("md5").unwrap();
    let config = ClientConfig::builder().checksum_choice(choice).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter()
            .any(|a| a.to_string_lossy().starts_with("--checksum-choice=")),
        "expected --checksum-choice=... in args: {args:?}"
    );
}

#[test]
fn includes_copy_links_flag() {
    let config = ClientConfig::builder().copy_links(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    assert!(flags.contains('L'), "expected 'L' in flags: {flags}");
}

#[test]
fn includes_keep_dirlinks_flag() {
    let config = ClientConfig::builder().keep_dirlinks(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    assert!(flags.contains('K'), "expected 'K' in flags: {flags}");
}

#[test]
fn includes_executability_flag() {
    let config = ClientConfig::builder().executability(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    assert!(flags.contains('E'), "expected 'E' in flags: {flags}");
}

/// upstream: options.c:2674 - 'E' is only sent when preserve_perms is false.
#[test]
fn executability_suppressed_when_permissions_set() {
    let config = ClientConfig::builder()
        .permissions(true)
        .executability(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    assert!(flags.contains('p'), "expected 'p' in flags: {flags}");
    assert!(
        !flags.contains('E'),
        "'E' must not appear when 'p' is set: {flags}"
    );
}

#[test]
fn includes_fuzzy_flag() {
    let config = ClientConfig::builder().fuzzy(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    assert!(flags.contains('y'), "expected 'y' in flags: {flags}");
}

#[test]
fn includes_double_fuzzy_flag() {
    let config = ClientConfig::builder().fuzzy_level(2).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    assert!(flags.contains("yy"), "expected 'yy' in flags: {flags}");
}

#[test]
fn includes_prune_empty_dirs_flag() {
    let config = ClientConfig::builder().prune_empty_dirs(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    assert!(flags.contains('m'), "expected 'm' in flags: {flags}");
}

#[test]
fn includes_verbosity_flags() {
    let config = ClientConfig::builder().verbosity(3).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    let v_count = flags.chars().filter(|c| *c == 'v').count();
    assert_eq!(v_count, 3, "expected 3 'v' chars in flags: {flags}");
}

#[test]
fn includes_backup_related_args() {
    let config = ClientConfig::builder()
        .backup(true)
        .backup_directory(Some("/backup"))
        .backup_suffix(Some(".bak"))
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--backup"),
        "expected --backup in args: {args:?}"
    );
    assert!(
        args.iter()
            .any(|a| a.to_string_lossy() == "--backup-dir=/backup"),
        "expected --backup-dir=/backup in args: {args:?}"
    );
    assert!(
        args.iter().any(|a| a.to_string_lossy() == "--suffix=.bak"),
        "expected --suffix=.bak in args: {args:?}"
    );
}

#[test]
fn includes_link_dest_via_reference_directories() {
    let config = ClientConfig::builder().link_destination("/prev").build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter()
            .any(|a| a.to_string_lossy() == "--link-dest=/prev"),
        "expected --link-dest=/prev in args: {args:?}"
    );
}

#[test]
fn includes_fake_super_long_arg() {
    let config = ClientConfig::builder().fake_super(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--fake-super"),
        "expected --fake-super in args: {args:?}"
    );
}

#[test]
fn includes_delay_updates_long_arg() {
    let config = ClientConfig::builder().delay_updates(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--delay-updates"),
        "expected --delay-updates in args: {args:?}"
    );
}

#[test]
fn includes_remove_source_files_long_arg() {
    let config = ClientConfig::builder().remove_source_files(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--remove-source-files"),
        "expected --remove-source-files in args: {args:?}"
    );
}

#[test]
fn includes_size_only_long_arg() {
    let config = ClientConfig::builder().size_only(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--size-only"),
        "expected --size-only in args: {args:?}"
    );
}

#[test]
fn includes_no_implied_dirs_when_disabled() {
    let config = ClientConfig::builder().implied_dirs(false).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--no-implied-dirs"),
        "expected --no-implied-dirs in args: {args:?}"
    );
}

#[test]
fn omits_no_implied_dirs_when_default() {
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        !args.iter().any(|a| a == "--no-implied-dirs"),
        "unexpected --no-implied-dirs in args: {args:?}"
    );
}

#[test]
fn includes_dry_run_flag() {
    let config = ClientConfig::builder().dry_run(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let flags = args[2].to_string_lossy();
    assert!(flags.contains('n'), "expected 'n' in flags: {flags}");
}

#[test]
fn secluded_invocation_disabled_returns_normal_args() {
    let config = ClientConfig::builder().protect_args(None).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let secluded = builder.build_secluded(&["/path"]);

    // When secluded-args is not enabled, stdin_args should be empty
    assert!(
        secluded.stdin_args.is_empty(),
        "stdin_args should be empty when protect_args is off"
    );
    // command_line_args should contain the full invocation
    assert!(
        secluded.command_line_args.iter().any(|a| a == "/path"),
        "command_line_args should contain the remote path"
    );
}

#[test]
fn secluded_invocation_enabled_produces_minimal_command_line() {
    let config = ClientConfig::builder().protect_args(Some(true)).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let secluded = builder.build_secluded(&["/path/to/files"]);

    // Command line should be minimal
    let cmd_strs: Vec<String> = secluded
        .command_line_args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(cmd_strs.contains(&"rsync".to_owned()));
    assert!(cmd_strs.contains(&"--server".to_owned()));
    assert!(cmd_strs.contains(&"-s".to_owned()));
    assert!(cmd_strs.contains(&".".to_owned()));

    // Command line should NOT contain the remote path
    assert!(
        !cmd_strs.contains(&"/path/to/files".to_owned()),
        "command line should not contain remote path in secluded mode"
    );

    // stdin_args should contain the full arguments
    assert!(
        !secluded.stdin_args.is_empty(),
        "stdin_args should not be empty when protect_args is on"
    );
    assert!(
        secluded.stdin_args.iter().any(|a| a == "/path/to/files"),
        "stdin_args should contain the remote path"
    );
}

#[test]
fn secluded_invocation_pull_includes_sender_flag() {
    let config = ClientConfig::builder().protect_args(Some(true)).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let secluded = builder.build_secluded(&["/remote/src"]);

    let cmd_strs: Vec<String> = secluded
        .command_line_args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(
        cmd_strs.contains(&"--sender".to_owned()),
        "pull secluded invocation should include --sender on command line"
    );
    assert!(
        cmd_strs.contains(&"-s".to_owned()),
        "secluded invocation should include -s flag"
    );

    // stdin_args should also include --sender
    assert!(
        secluded.stdin_args.iter().any(|a| a == "--sender"),
        "stdin_args should include --sender for pull"
    );
}

#[test]
fn secluded_invocation_stdin_args_contain_flag_string() {
    let config = ClientConfig::builder()
        .protect_args(Some(true))
        .recursive(true)
        .times(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let secluded = builder.build_secluded(&["/data"]);

    // stdin_args should contain the flag string
    let has_flags = secluded
        .stdin_args
        .iter()
        .any(|a| a.starts_with('-') && a.contains('r') && a.contains('t'));
    assert!(
        has_flags,
        "stdin_args should contain flag string with 'r' and 't': {:?}",
        secluded.stdin_args
    );
}

#[test]
fn secluded_invocation_explicitly_disabled_returns_normal() {
    let config = ClientConfig::builder().protect_args(Some(false)).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let secluded = builder.build_secluded(&["/path"]);

    assert!(
        secluded.stdin_args.is_empty(),
        "stdin_args should be empty when protect_args is explicitly false"
    );
}

//
// These tests verify that every CLI flag supported by the builder is correctly
// forwarded to the remote server invocation. Each test constructs a config with
// a specific flag, builds the invocation, and asserts the expected argument
// appears in the output.

/// Helper: builds a push (Sender) invocation and returns the args vector.
fn build_sender_args(config: &ClientConfig) -> Vec<String> {
    let builder = RemoteInvocationBuilder::new(config, RemoteRole::Sender);
    builder
        .build("/path")
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// Helper: finds the compact flag string in an args vector.
///
/// The flag string starts with `-` but not `--`, and is not the capability
/// string (`-e.xxx`). This handles variable positioning caused by
/// `--ignore-errors` and `--fsync` appearing before the flag string.
fn find_flag_string(args: &[String]) -> &str {
    args.iter()
        .find(|a| a.starts_with('-') && !a.starts_with("--") && !a.starts_with("-e."))
        .map(|s| s.as_str())
        .expect("flag string not found in args")
}

/// Helper: extracts the compact flag string from a push (Sender) invocation.
fn sender_flag_string(config: &ClientConfig) -> String {
    let args = build_sender_args(config);
    find_flag_string(&args).to_owned()
}

/// Helper: builds a pull (Receiver) invocation and returns the args vector.
fn build_receiver_args(config: &ClientConfig) -> Vec<String> {
    let builder = RemoteInvocationBuilder::new(config, RemoteRole::Receiver);
    builder
        .build("/path")
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// Helper: extracts the compact flag string from a pull (Receiver) invocation.
fn receiver_flag_string(config: &ClientConfig) -> String {
    let args = build_receiver_args(config);
    find_flag_string(&args).to_owned()
}

#[test]
fn default_config_produces_expected_flags() {
    // ClientConfig::builder() sets recursive=true by default, and
    // whole_file() returns true when the raw option is None (auto-detect).
    let config = ClientConfig::builder().build();
    let flags = sender_flag_string(&config);
    assert!(
        flags.contains('r'),
        "default builder enables recursive: {flags}"
    );
    // upstream: options.c:2644-2648 - 'W' is only sent when explicitly set.
    // The default for remote transfers is no-whole-file.
    assert!(
        !flags.contains('W'),
        "default whole_file (auto) must not send 'W': {flags}"
    );
}

#[test]
fn default_config_has_no_long_form_args() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    for arg in &args {
        if arg.starts_with("--") && arg != "--server" {
            panic!("default config should not emit long-form arg: {arg}");
        }
    }
}

#[test]
fn archive_mode_flags_all_present() {
    // -a is equivalent to -rlptgoD
    let config = ClientConfig::builder()
        .recursive(true)
        .links(true)
        .permissions(true)
        .times(true)
        .group(true)
        .owner(true)
        .devices(true)
        .specials(true)
        .build();

    let flags = sender_flag_string(&config);
    assert!(flags.contains('r'), "archive: missing 'r' in {flags}");
    assert!(flags.contains('l'), "archive: missing 'l' in {flags}");
    assert!(flags.contains('p'), "archive: missing 'p' in {flags}");
    assert!(flags.contains('t'), "archive: missing 't' in {flags}");
    assert!(flags.contains('g'), "archive: missing 'g' in {flags}");
    assert!(flags.contains('o'), "archive: missing 'o' in {flags}");
    assert!(flags.contains('D'), "archive: missing 'D' in {flags}");
}

#[test]
fn includes_links_flag() {
    let config = ClientConfig::builder().links(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('l'), "expected 'l' in flags: {flags}");
}

#[test]
fn includes_copy_dirlinks_flag() {
    let config = ClientConfig::builder().copy_dirlinks(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('k'), "expected 'k' in flags: {flags}");
}

#[test]
fn includes_devices_flag() {
    let config = ClientConfig::builder().devices(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('D'), "expected 'D' in flags: {flags}");
}

#[test]
fn includes_specials_flag() {
    let config = ClientConfig::builder().specials(true).build();
    let flags = sender_flag_string(&config);
    assert!(
        flags.contains('D'),
        "expected 'D' for specials in flags: {flags}"
    );
}

#[test]
fn devices_and_specials_produce_single_d_flag() {
    let config = ClientConfig::builder().devices(true).specials(true).build();
    let flags = sender_flag_string(&config);
    let d_count = flags.chars().filter(|c| *c == 'D').count();
    assert_eq!(
        d_count, 1,
        "devices+specials should produce single 'D', got {d_count} in {flags}"
    );
}

#[test]
fn includes_atimes_flag() {
    let config = ClientConfig::builder().atimes(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('U'), "expected 'U' in flags: {flags}");
}

#[test]
fn includes_hard_links_flag() {
    let config = ClientConfig::builder().hard_links(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('H'), "expected 'H' in flags: {flags}");
}

#[test]
fn numeric_ids_is_long_form_not_in_flag_string() {
    // upstream: numeric_ids is sent as --numeric-ids long-form arg, never as 'n' flag.
    // 'n' in compact flags means dry_run.
    let config = ClientConfig::builder().numeric_ids(true).build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('n'),
        "'n' should NOT appear for numeric_ids: {flags}"
    );
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--numeric-ids"),
        "expected --numeric-ids in long-form args: {args:?}"
    );
}

#[test]
fn includes_whole_file_flag() {
    let config = ClientConfig::builder().whole_file(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('W'), "expected 'W' in flags: {flags}");
}

#[test]
fn includes_one_file_system_flag() {
    let config = ClientConfig::builder().one_file_system(1).build();
    let flags = sender_flag_string(&config);
    let x_count = flags.chars().filter(|c| *c == 'x').count();
    assert_eq!(x_count, 1, "expected 1 'x' in flags: {flags}");
}

#[test]
fn includes_double_one_file_system_flag() {
    let config = ClientConfig::builder().one_file_system(2).build();
    let flags = sender_flag_string(&config);
    let x_count = flags.chars().filter(|c| *c == 'x').count();
    assert_eq!(x_count, 2, "expected 2 'x' in flags: {flags}");
}

#[test]
fn includes_relative_paths_flag() {
    let config = ClientConfig::builder().relative_paths(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('R'), "expected 'R' in flags: {flags}");
}

#[test]
fn includes_partial_flag() {
    let config = ClientConfig::builder().partial(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('P'), "expected 'P' in flags: {flags}");
}

#[test]
fn includes_update_flag() {
    let config = ClientConfig::builder().update(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('u'), "expected 'u' in flags: {flags}");
}

#[test]
fn includes_crtimes_flag() {
    let config = ClientConfig::builder().crtimes(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('N'), "expected 'N' in flags: {flags}");
}

#[test]
fn includes_checksum_flag() {
    let config = ClientConfig::builder().checksum(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('c'), "expected 'c' in flags: {flags}");
}

#[test]
fn includes_sparse_flag() {
    let config = ClientConfig::builder().sparse(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('S'), "expected 'S' in flags: {flags}");
}

#[test]
fn delete_mode_is_long_form_not_in_flag_string() {
    // upstream: delete is sent as --delete-* long-form arg, never as 'd' flag.
    // 'd' in compact flags means --dirs (xfer_dirs without recursion).
    let config = ClientConfig::builder().delete_before(true).build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('d'),
        "'d' should NOT appear for delete mode: {flags}"
    );
}

#[test]
fn delete_excluded_is_long_form_not_in_flag_string() {
    // upstream: delete_excluded is sent as --delete-excluded long-form arg.
    let config = ClientConfig::builder().delete_excluded(true).build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('d'),
        "'d' should NOT appear for delete_excluded: {flags}"
    );
}

#[cfg(all(any(unix, windows), feature = "acl"))]
#[test]
fn includes_acl_flag() {
    let config = ClientConfig::builder().acls(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('A'), "expected 'A' in flags: {flags}");
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn includes_xattr_flag() {
    let config = ClientConfig::builder().xattrs(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('X'), "expected 'X' in flags: {flags}");
}

#[test]
fn includes_delete_during_long_arg() {
    let config = ClientConfig::builder().delete_during().build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--delete-during"),
        "expected --delete-during in args: {args:?}"
    );
}

#[test]
fn includes_delete_delay_long_arg() {
    let config = ClientConfig::builder().delete_delay(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--delete-delay"),
        "expected --delete-delay in args: {args:?}"
    );
}

#[test]
fn includes_force_long_arg() {
    let config = ClientConfig::builder().force_replacements(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--force"),
        "expected --force in args: {args:?}"
    );
}

#[test]
fn includes_max_delete_long_arg() {
    let config = ClientConfig::builder().max_delete(Some(100)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--max-delete=100"),
        "expected --max-delete=100 in args: {args:?}"
    );
}

#[test]
fn includes_max_size_long_arg() {
    let config = ClientConfig::builder().max_file_size(Some(1048576)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--max-size=1048576"),
        "expected --max-size=1048576 in args: {args:?}"
    );
}

#[test]
fn includes_min_size_long_arg() {
    let config = ClientConfig::builder().min_file_size(Some(1024)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--min-size=1024"),
        "expected --min-size=1024 in args: {args:?}"
    );
}

#[test]
fn includes_modify_window_long_arg() {
    let config = ClientConfig::builder().modify_window(Some(2)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--modify-window=2"),
        "expected --modify-window=2 in args: {args:?}"
    );
}

#[test]
fn includes_compress_level_long_arg() {
    let config = ClientConfig::builder()
        .compress(true)
        .compression_level(Some(compress::zlib::CompressionLevel::Best))
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--compress-level=9"),
        "expected --compress-level=9 in args: {args:?}"
    );
}

#[test]
fn includes_compress_level_fast() {
    let config = ClientConfig::builder()
        .compress(true)
        .compression_level(Some(compress::zlib::CompressionLevel::Fast))
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--compress-level=1"),
        "expected --compress-level=1 in args: {args:?}"
    );
}

#[test]
fn includes_compress_level_default() {
    let config = ClientConfig::builder()
        .compress(true)
        .compression_level(Some(compress::zlib::CompressionLevel::Default))
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--compress-level=6"),
        "expected --compress-level=6 in args: {args:?}"
    );
}

#[test]
fn includes_old_compress_for_explicit_zlib() {
    // upstream: options.c:2802 - explicit zlib sent as --old-compress
    let config = ClientConfig::builder()
        .compression_algorithm(CompressionAlgorithm::Zlib)
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--old-compress"),
        "expected --old-compress for explicit zlib in args: {args:?}"
    );
}

#[test]
fn omits_compress_choice_when_not_explicitly_set() {
    // Default config (no explicit --compress-choice) should not send any
    // compress-choice argument, even if the default algorithm is non-zlib.
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--compress-choice=")
            || a == "--old-compress"
            || a == "--new-compress"),
        "should not emit compress-choice args when not explicitly set: {args:?}"
    );
}

#[test]
fn includes_compress_choice_for_explicit_default_algorithm() {
    // Even when the user explicitly chooses the default algorithm (e.g.
    // --compress-choice=zstd when zstd is the default), it should still
    // be forwarded to the remote side.
    let config = ClientConfig::builder()
        .compression_algorithm(CompressionAlgorithm::default_algorithm())
        .build();
    let args = build_sender_args(&config);
    let has_compress_arg = args.iter().any(|a| {
        a.starts_with("--compress-choice=") || a == "--old-compress" || a == "--new-compress"
    });
    assert!(
        has_compress_arg,
        "explicitly choosing the default algorithm should still forward it: {args:?}"
    );
}

#[cfg(feature = "lz4")]
#[test]
fn includes_compress_choice_for_lz4() {
    let config = ClientConfig::builder()
        .compression_algorithm(CompressionAlgorithm::Lz4)
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter()
            .any(|a| a.starts_with("--compress-choice=") && a.contains("lz4")),
        "expected --compress-choice=lz4 in args: {args:?}"
    );
}

#[test]
fn includes_new_compress_for_explicit_zlibx() {
    // upstream: options.c:2800 - zlibx sent as --new-compress
    let config = ClientConfig::builder()
        .compression_algorithm(compress::algorithm::CompressionAlgorithm::Zlib)
        .build();
    // Note: the compress crate maps both "zlib" and "zlibx" to Zlib.
    // The wire name of CompressionAlgorithm::Zlib is "zlib", so this
    // should emit --old-compress, not --new-compress.
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--old-compress"),
        "expected --old-compress for zlib algorithm: {args:?}"
    );
}

#[test]
fn includes_block_size_long_arg() {
    use std::num::NonZeroU32;
    let config = ClientConfig::builder()
        .block_size_override(Some(NonZeroU32::new(8192).unwrap()))
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--block-size=8192"),
        "expected --block-size=8192 in args: {args:?}"
    );
}

#[test]
fn includes_temp_dir_long_arg() {
    let config = ClientConfig::builder()
        .temp_directory(Some("/tmp/staging"))
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--temp-dir=/tmp/staging"),
        "expected --temp-dir=/tmp/staging in args: {args:?}"
    );
}

#[test]
fn includes_append_long_arg() {
    let config = ClientConfig::builder().append(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--append"),
        "expected --append in args: {args:?}"
    );
}

#[test]
fn append_verify_via_builder_emits_append() {
    // The builder's append_verify(true) sets append=true internally,
    // so the invocation emits --append (the append() check comes first).
    // This mirrors upstream behavior where --append-verify implies --append.
    let config = ClientConfig::builder().append_verify(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--append"),
        "append_verify via builder should produce --append: {args:?}"
    );
}

#[test]
fn append_verify_emits_append_because_builder_sets_both() {
    // The builder's append_verify(true) sets both append=true and
    // append_verify=true. The invocation code checks append() first,
    // so --append is emitted. To get --append-verify alone, one would
    // need to set append_verify without append (not possible via builder).
    let config = ClientConfig::builder().append_verify(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--append"),
        "append_verify via builder should emit --append: {args:?}"
    );
}

#[test]
fn includes_copy_unsafe_links_long_arg() {
    let config = ClientConfig::builder().copy_unsafe_links(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--copy-unsafe-links"),
        "expected --copy-unsafe-links in args: {args:?}"
    );
}

#[test]
fn includes_safe_links_long_arg() {
    let config = ClientConfig::builder().safe_links(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--safe-links"),
        "expected --safe-links in args: {args:?}"
    );
}

#[test]
fn includes_munge_links_long_arg() {
    let config = ClientConfig::builder().munge_links(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--munge-links"),
        "expected --munge-links in args: {args:?}"
    );
}

#[test]
fn includes_ignore_times_long_arg() {
    let config = ClientConfig::builder().ignore_times(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--ignore-times"),
        "expected --ignore-times in args: {args:?}"
    );
}

#[test]
fn includes_ignore_existing_long_arg() {
    let config = ClientConfig::builder().ignore_existing(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--ignore-existing"),
        "expected --ignore-existing in args: {args:?}"
    );
}

#[test]
fn includes_existing_only_long_arg() {
    let config = ClientConfig::builder().existing_only(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--existing"),
        "expected --existing in args: {args:?}"
    );
}

#[test]
fn includes_omit_dir_times_long_arg() {
    let config = ClientConfig::builder().omit_dir_times(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--omit-dir-times"),
        "expected --omit-dir-times in args: {args:?}"
    );
}

#[test]
fn includes_omit_link_times_long_arg() {
    let config = ClientConfig::builder().omit_link_times(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--omit-link-times"),
        "expected --omit-link-times in args: {args:?}"
    );
}

#[test]
fn includes_compare_dest_long_arg() {
    let config = ClientConfig::builder()
        .compare_destination("/tmp/compare")
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--compare-dest=/tmp/compare"),
        "expected --compare-dest=/tmp/compare in args: {args:?}"
    );
}

#[test]
fn includes_copy_dest_long_arg() {
    let config = ClientConfig::builder()
        .copy_destination("/tmp/copy")
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--copy-dest=/tmp/copy"),
        "expected --copy-dest=/tmp/copy in args: {args:?}"
    );
}

#[test]
fn includes_copy_devices_long_arg() {
    let config = ClientConfig::builder().copy_devices(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--copy-devices"),
        "expected --copy-devices in args: {args:?}"
    );
}

#[test]
fn includes_write_devices_long_arg() {
    let config = ClientConfig::builder().write_devices(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--write-devices"),
        "expected --write-devices in args: {args:?}"
    );
}

#[test]
fn includes_open_noatime_long_arg() {
    let config = ClientConfig::builder().open_noatime(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--open-noatime"),
        "expected --open-noatime in args: {args:?}"
    );
}

#[test]
fn includes_preallocate_long_arg() {
    let config = ClientConfig::builder().preallocate(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--preallocate"),
        "expected --preallocate in args: {args:?}"
    );
}

#[test]
fn custom_rsync_path_used_as_program_name() {
    let config = ClientConfig::builder()
        .set_rsync_path("/opt/rsync/bin/rsync")
        .build();
    let args = build_sender_args(&config);
    assert_eq!(
        args[0], "/opt/rsync/bin/rsync",
        "first arg should be custom rsync path"
    );
}

#[test]
fn default_rsync_path_is_rsync() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert_eq!(args[0], "rsync", "default program name should be 'rsync'");
}

#[test]
fn capability_string_present_in_sender_args() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    let expected = build_capability_string(true);
    assert!(
        args.iter().any(|a| a == &expected),
        "expected capability string {expected} in args: {args:?}"
    );
}

#[test]
fn capability_string_present_in_receiver_args() {
    let config = ClientConfig::builder().build();
    let args = build_receiver_args(&config);
    let expected = build_capability_string(true);
    assert!(
        args.iter().any(|a| a == &expected),
        "expected capability string {expected} in args: {args:?}"
    );
}

#[test]
fn dot_placeholder_precedes_remote_path_in_sender() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    let dot_idx = args.iter().position(|a| a == ".").unwrap();
    let path_idx = args.iter().position(|a| a == "/path").unwrap();
    assert_eq!(
        path_idx,
        dot_idx + 1,
        "remote path should immediately follow '.' placeholder"
    );
}

#[test]
fn dot_placeholder_precedes_remote_path_in_receiver() {
    let config = ClientConfig::builder().build();
    let args = build_receiver_args(&config);
    let dot_idx = args.iter().position(|a| a == ".").unwrap();
    let path_idx = args.iter().position(|a| a == "/path").unwrap();
    assert_eq!(
        path_idx,
        dot_idx + 1,
        "remote path should immediately follow '.' placeholder"
    );
}

#[test]
fn build_with_multiple_paths() {
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args: Vec<String> = builder
        .build_with_paths(&["/src1", "/src2", "/src3"])
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let dot_idx = args.iter().position(|a| a == ".").unwrap();
    assert_eq!(args[dot_idx + 1], "/src1");
    assert_eq!(args[dot_idx + 2], "/src2");
    assert_eq!(args[dot_idx + 3], "/src3");
}

#[test]
fn receiver_flag_string_matches_sender_for_same_config() {
    let config = ClientConfig::builder()
        .recursive(true)
        .links(true)
        .times(true)
        .compress(true)
        .build();

    let s_flags = sender_flag_string(&config);
    let r_flags = receiver_flag_string(&config);
    assert_eq!(
        s_flags, r_flags,
        "flag strings should be identical for sender and receiver with same config"
    );
}

#[test]
fn only_one_delete_mode_emitted() {
    let config = ClientConfig::builder().delete_before(true).build();
    let args = build_sender_args(&config);
    let delete_args: Vec<&String> = args.iter().filter(|a| a.starts_with("--delete-")).collect();
    assert_eq!(
        delete_args.len(),
        1,
        "only one --delete-* arg should be emitted, got: {delete_args:?}"
    );
    assert_eq!(delete_args[0], "--delete-before");
}

#[test]
fn delete_during_emits_only_during() {
    let config = ClientConfig::builder().delete_during().build();
    let args = build_sender_args(&config);
    let delete_args: Vec<&String> = args.iter().filter(|a| a.starts_with("--delete-")).collect();
    assert_eq!(delete_args.len(), 1);
    assert_eq!(delete_args[0], "--delete-during");
}

#[test]
fn delete_delay_emits_only_delay() {
    let config = ClientConfig::builder().delete_delay(true).build();
    let args = build_sender_args(&config);
    let delete_args: Vec<&String> = args
        .iter()
        .filter(|a| a.starts_with("--delete-") && !a.starts_with("--delete-excluded"))
        .collect();
    assert_eq!(delete_args.len(), 1);
    assert_eq!(delete_args[0], "--delete-delay");
}

#[test]
fn compress_with_level_emits_both_flag_and_level() {
    let config = ClientConfig::builder()
        .compress(true)
        .compression_level(Some(compress::zlib::CompressionLevel::Best))
        .build();
    let flags = sender_flag_string(&config);
    assert!(
        flags.contains('z'),
        "expected 'z' flag for compress: {flags}"
    );
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--compress-level=9"),
        "expected --compress-level=9 in args: {args:?}"
    );
}

#[test]
fn partial_dir_also_emits_partial_flag_string() {
    let config = ClientConfig::builder()
        .partial_directory(Some(".rsync-partial"))
        .build();
    let flags = sender_flag_string(&config);
    // partial_directory sets partial=true, which should emit 'P' flag
    assert!(
        flags.contains('P'),
        "partial_directory should also set 'P' flag: {flags}"
    );
}

#[test]
fn backup_without_dir_or_suffix_emits_only_backup() {
    let config = ClientConfig::builder().backup(true).build();
    let args = build_sender_args(&config);
    assert!(args.iter().any(|a| a == "--backup"));
    assert!(!args.iter().any(|a| a.starts_with("--backup-dir=")));
    assert!(!args.iter().any(|a| a.starts_with("--suffix=")));
}

#[test]
fn omits_compress_flag_when_disabled() {
    let config = ClientConfig::builder().build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('z'),
        "should not contain 'z' when compress is off: {flags}"
    );
}

#[test]
fn omits_recursive_flag_when_explicitly_disabled() {
    let config = ClientConfig::builder().recursive(false).build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('r'),
        "should not contain 'r' when recursive=false: {flags}"
    );
}

#[test]
fn omits_delete_long_args_when_disabled() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--delete")),
        "should not emit --delete-* when delete is disabled: {args:?}"
    );
}

#[test]
fn omits_timeout_when_default() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--timeout=")),
        "should not emit --timeout= when default: {args:?}"
    );
}

#[test]
fn omits_bwlimit_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--bwlimit=")),
        "should not emit --bwlimit= when none: {args:?}"
    );
}

#[test]
fn omits_block_size_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--block-size=")),
        "should not emit --block-size= when none: {args:?}"
    );
}

#[test]
fn omits_max_delete_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--max-delete=")),
        "should not emit --max-delete= when none: {args:?}"
    );
}

#[test]
fn omits_max_size_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--max-size=")),
        "should not emit --max-size= when none: {args:?}"
    );
}

#[test]
fn omits_min_size_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--min-size=")),
        "should not emit --min-size= when none: {args:?}"
    );
}

#[test]
fn omits_modify_window_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--modify-window=")),
        "should not emit --modify-window= when none: {args:?}"
    );
}

#[test]
fn omits_compress_level_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--compress-level=")),
        "should not emit --compress-level= when none: {args:?}"
    );
}

#[test]
fn omits_checksum_choice_when_auto() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--checksum-choice=")),
        "should not emit --checksum-choice= when auto: {args:?}"
    );
}

#[test]
fn omits_partial_dir_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--partial-dir=")),
        "should not emit --partial-dir= when none: {args:?}"
    );
}

#[test]
fn omits_temp_dir_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--temp-dir=")),
        "should not emit --temp-dir= when none: {args:?}"
    );
}

#[test]
fn omits_backup_args_when_disabled() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a == "--backup"),
        "should not emit --backup when disabled: {args:?}"
    );
}

#[test]
fn omits_reference_dirs_when_empty() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--compare-dest=")
            || a.starts_with("--copy-dest=")
            || a.starts_with("--link-dest=")),
        "should not emit reference dir args when empty: {args:?}"
    );
}

#[test]
fn all_flags_enabled_produces_valid_invocation() {
    use crate::client::config::{BandwidthLimit, StrongChecksumChoice};
    use std::num::{NonZeroU32, NonZeroU64};

    let choice = StrongChecksumChoice::parse("md5").unwrap();
    let config = ClientConfig::builder()
        // Short flags
        .links(true)
        .copy_links(true)
        .copy_dirlinks(true)
        .keep_dirlinks(true)
        .owner(true)
        .group(true)
        .devices(true)
        .specials(true)
        .times(true)
        .atimes(true)
        .permissions(true)
        .executability(true)
        .recursive(true)
        .compress(true)
        .checksum(true)
        .hard_links(true)
        .acls(true)
        .xattrs(true)
        .numeric_ids(true)
        .dry_run(true)
        .delete_before(true)
        .whole_file(true)
        .sparse(true)
        .fuzzy(true)
        .one_file_system(2)
        .relative_paths(true)
        .partial(true)
        .update(true)
        .crtimes(true)
        .prune_empty_dirs(true)
        .verbosity(2)
        // Long-form args
        .ignore_errors(true)
        .fsync(true)
        .delete_excluded(true)
        .force_replacements(true)
        .max_delete(Some(50))
        .max_file_size(Some(1000000))
        .min_file_size(Some(100))
        .modify_window(Some(1))
        .compression_level(Some(compress::zlib::CompressionLevel::Best))
        .compression_algorithm(CompressionAlgorithm::Zlib)
        .checksum_choice(choice)
        .block_size_override(Some(NonZeroU32::new(4096).unwrap()))
        .timeout(TransferTimeout::Seconds(NonZeroU64::new(60).unwrap()))
        .bandwidth_limit(Some(BandwidthLimit::from_bytes_per_second(
            NonZeroU64::new(1024).unwrap(),
        )))
        .partial_directory(Some(".partial"))
        .temp_directory(Some("/tmp/rsync"))
        .inplace(true)
        .copy_unsafe_links(true)
        .safe_links(true)
        .munge_links(true)
        .size_only(true)
        .ignore_times(true)
        .ignore_existing(true)
        .existing_only(true)
        .remove_source_files(true)
        .implied_dirs(false)
        .fake_super(true)
        .omit_dir_times(true)
        .omit_link_times(true)
        .delay_updates(true)
        .backup(true)
        .backup_directory(Some("/backup"))
        .backup_suffix(Some(".bak"))
        .link_destination("/prev")
        .compare_destination("/cmp")
        .copy_destination("/cpd")
        .copy_devices(true)
        .write_devices(true)
        .open_noatime(true)
        .preallocate(true)
        .set_rsync_path("/custom/rsync")
        .build();

    let args = build_sender_args(&config);

    // Verify structural integrity: program name, --server, flags, capability, dot, path
    assert_eq!(args[0], "/custom/rsync");
    assert_eq!(args[1], "--server");

    // Verify --ignore-errors and --fsync are present
    assert!(args.contains(&"--ignore-errors".to_owned()));
    assert!(args.contains(&"--fsync".to_owned()));

    // Verify flag string contains all expected single-char flags
    let flags = find_flag_string(&args);
    for (ch, name) in [
        ('l', "links"),
        ('L', "copy_links"),
        ('k', "copy_dirlinks"),
        ('K', "keep_dirlinks"),
        ('o', "owner"),
        ('g', "group"),
        ('D', "devices/specials"),
        ('t', "times"),
        ('U', "atimes"),
        ('p', "permissions"),
        // upstream: options.c:2674 - 'E' is only sent when preserve_perms
        // is false (else-if), so it is absent when 'p' is also set.
        ('r', "recursive"),
        ('z', "compress"),
        ('c', "checksum"),
        ('H', "hard_links"),
        ('n', "dry_run"),
        ('W', "whole_file"),
        ('S', "sparse"),
        ('y', "fuzzy"),
        ('R', "relative_paths"),
        ('P', "partial"),
        ('u', "update"),
        ('N', "crtimes"),
        ('m', "prune_empty_dirs"),
    ] {
        assert!(
            flags.contains(ch),
            "all-flags test: missing '{ch}' ({name}) in flags: {flags}"
        );
    }

    // Verify 'x' count
    let x_count = flags.chars().filter(|c| *c == 'x').count();
    assert_eq!(x_count, 2, "expected 2 'x' flags for one_file_system=2");

    // Verify 'v' count
    let v_count = flags.chars().filter(|c| *c == 'v').count();
    assert_eq!(v_count, 2, "expected 2 'v' flags for verbosity=2");

    // Verify all long-form args
    let expected_long_args = [
        "--delete-before",
        "--delete-excluded",
        "--force",
        "--numeric-ids",
        "--max-delete=50",
        "--max-size=1000000",
        "--min-size=100",
        "--modify-window=1",
        "--compress-level=9",
        "--block-size=4096",
        "--timeout=60",
        "--inplace",
        "--copy-unsafe-links",
        "--safe-links",
        "--munge-links",
        "--size-only",
        "--ignore-times",
        "--ignore-existing",
        "--existing",
        "--remove-source-files",
        "--no-implied-dirs",
        "--fake-super",
        "--omit-dir-times",
        "--omit-link-times",
        "--delay-updates",
        "--backup",
        "--copy-devices",
        "--write-devices",
        "--open-noatime",
        "--preallocate",
    ];

    for expected in expected_long_args {
        assert!(
            args.iter().any(|a| a == expected),
            "all-flags test: missing {expected} in args: {args:?}"
        );
    }

    // Verify args with values using prefix matching
    // upstream: options.c:2802 - explicit zlib is sent as --old-compress
    assert!(
        args.iter().any(|a| a == "--old-compress"),
        "all-flags test: expected --old-compress for explicit zlib: {args:?}"
    );

    let expected_prefixed = [
        "--checksum-choice=",
        "--bwlimit=",
        "--partial-dir=",
        "--temp-dir=",
        "--backup-dir=",
        "--suffix=",
        "--link-dest=",
        "--compare-dest=",
        "--copy-dest=",
    ];
    for prefix in expected_prefixed {
        assert!(
            args.iter().any(|a| a.starts_with(prefix)),
            "all-flags test: missing arg with prefix {prefix} in args: {args:?}"
        );
    }

    // Verify capability string and structural elements
    assert!(args.contains(&build_capability_string(true)));
    assert!(args.contains(&".".to_owned()));
    assert!(args.contains(&"/path".to_owned()));
}

#[test]
fn local_absolute_path_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("/tmp/foo")));
}

#[test]
fn local_relative_path_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("./relative/path")));
}

#[test]
fn local_bare_filename_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("file.txt")));
}

#[test]
fn local_path_with_colon_after_slash_is_not_remote() {
    // Colon appears after a slash, so the before-colon part contains '/'.
    // upstream: main.c - only treat as remote if no slash before colon.
    assert!(!operand_is_remote(OsStr::new("/foo:bar")));
}

#[test]
fn local_path_with_colon_after_backslash_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("dir\\sub:file")));
}

#[test]
fn local_path_nested_colon_after_slash_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("/a/b/c:d")));
}

#[test]
fn local_dot_path_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new(".")));
}

#[test]
fn local_parent_path_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("..")));
}

#[cfg(windows)]
#[test]
fn windows_drive_letter_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("C:\\Windows\\path")));
}

#[cfg(windows)]
#[test]
fn windows_drive_letter_forward_slash_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("D:/Users/test")));
}

#[cfg(not(windows))]
#[test]
fn single_letter_colon_on_unix_is_remote() {
    // On Unix, "C:" looks like host:path with empty path - treated as remote.
    // Only Windows has the drive letter exemption.
    assert!(operand_is_remote(OsStr::new("C:")));
}

#[test]
fn rsync_url_is_remote() {
    assert!(operand_is_remote(OsStr::new("rsync://host/module/path")));
}

#[test]
fn rsync_url_with_user_is_remote() {
    assert!(operand_is_remote(OsStr::new("rsync://user@host/module")));
}

#[test]
fn rsync_url_bare_host_is_remote() {
    assert!(operand_is_remote(OsStr::new("rsync://host")));
}

#[test]
fn ssh_url_is_remote() {
    assert!(operand_is_remote(OsStr::new("ssh://host/path")));
}

#[test]
fn ssh_url_with_user_is_remote() {
    assert!(operand_is_remote(OsStr::new("ssh://user@host/path")));
}

#[test]
fn ssh_url_with_port_is_remote() {
    assert!(operand_is_remote(OsStr::new("ssh://user@host:22/path")));
}

#[test]
fn ssh_url_bare_host_is_remote() {
    assert!(operand_is_remote(OsStr::new("ssh://host")));
}

#[test]
fn host_colon_path_is_remote() {
    assert!(operand_is_remote(OsStr::new("host:path")));
}

#[test]
fn user_at_host_colon_path_is_remote() {
    assert!(operand_is_remote(OsStr::new("user@host:path")));
}

#[test]
fn host_colon_empty_path_is_remote() {
    assert!(operand_is_remote(OsStr::new("host:")));
}

#[test]
fn ip_colon_path_is_remote() {
    assert!(operand_is_remote(OsStr::new("192.168.1.1:/data")));
}

#[test]
fn host_double_colon_module_is_remote() {
    assert!(operand_is_remote(OsStr::new("host::module")));
}

#[test]
fn user_at_host_double_colon_module_is_remote() {
    assert!(operand_is_remote(OsStr::new("user@host::module")));
}

#[test]
fn host_double_colon_module_path_is_remote() {
    assert!(operand_is_remote(OsStr::new("host::module/subdir")));
}

#[test]
fn empty_string_is_not_remote() {
    assert!(!operand_is_remote(OsStr::new("")));
}

#[test]
fn http_url_is_not_classified_as_ssh() {
    // "http://foo" has a colon, but the before-colon part ("http") contains
    // no '/' or '\', so it is treated as host:path (remote).
    // This matches upstream rsync behavior - http:// is not special-cased.
    assert!(operand_is_remote(OsStr::new("http://foo")));
}

#[test]
fn ftp_url_is_not_special_cased() {
    // Same as http:// - upstream rsync treats any host:path as remote.
    assert!(operand_is_remote(OsStr::new("ftp://foo")));
}

#[test]
fn uppercase_ssh_url_is_not_recognized() {
    // Only lowercase "ssh://" triggers the URL check. Uppercase "SSH://"
    // falls through to the colon-based check where "SSH" has no '/' or '\'
    // before the colon, so it is treated as host:path (remote) anyway.
    assert!(operand_is_remote(OsStr::new("SSH://host/path")));
}

#[test]
fn uppercase_rsync_url_is_not_recognized_as_url() {
    // "RSYNC://host" - "RSYNC" has no slash before colon, treated as remote.
    assert!(operand_is_remote(OsStr::new("RSYNC://host")));
}

#[test]
fn path_with_only_colons_double_is_remote() {
    // "::" contains double-colon, so it is treated as remote daemon syntax.
    assert!(operand_is_remote(OsStr::new("::")));
}

#[test]
fn path_with_single_colon_only_is_remote() {
    // ":" has a colon with empty before and after parts. Empty before has no
    // '/' or '\', so it falls through to remote classification.
    assert!(operand_is_remote(OsStr::new(":")));
}

#[test]
fn local_to_ssh_url_is_push() {
    let sources = vec![OsString::from("/tmp/data")];
    let dest = OsString::from("ssh://user@host/remote/path");
    let spec = determine_transfer_role(&sources, &dest).unwrap();
    assert_eq!(spec.role(), RemoteRole::Sender);
    match spec {
        TransferSpec::Push {
            local_sources,
            remote_dest,
        } => {
            assert_eq!(local_sources, vec!["/tmp/data"]);
            assert_eq!(remote_dest, "ssh://user@host/remote/path");
        }
        _ => panic!("expected Push for local -> ssh:// transfer"),
    }
}

#[test]
fn ssh_url_to_local_is_pull() {
    let sources = vec![OsString::from("ssh://host/remote/file")];
    let dest = OsString::from("/tmp/local");
    let spec = determine_transfer_role(&sources, &dest).unwrap();
    assert_eq!(spec.role(), RemoteRole::Receiver);
}

#[test]
fn rsync_url_to_local_is_pull() {
    let sources = vec![OsString::from("rsync://host/module/file")];
    let dest = OsString::from("./local_dir");
    let spec = determine_transfer_role(&sources, &dest).unwrap();
    assert_eq!(spec.role(), RemoteRole::Receiver);
}

#[test]
fn local_path_with_colon_after_slash_is_local_in_transfer() {
    // Both operands are local (colon after slash is not remote).
    let sources = vec![OsString::from("/data:backup/files")];
    let dest = OsString::from("/tmp/dest");
    let result = determine_transfer_role(&sources, &dest);
    assert!(
        result.is_err(),
        "two local operands should produce 'no remote operand' error"
    );
}

#[test]
fn daemon_double_colon_to_local_is_pull() {
    let sources = vec![OsString::from("host::module/path")];
    let dest = OsString::from("/tmp/local");
    let spec = determine_transfer_role(&sources, &dest).unwrap();
    assert_eq!(spec.role(), RemoteRole::Receiver);
}

// --iconv server-arg forwarding tests.
// upstream: options.c:2716-2723 - the post-comma half of iconv_opt is
// forwarded; without a comma the whole spec is forwarded; --iconv=- and the
// default forward nothing because options.c:2052-2054 nulls iconv_opt.

#[test]
fn iconv_unspecified_omits_iconv_arg() {
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");
    assert!(
        !args
            .iter()
            .any(|a| a.to_string_lossy().starts_with("--iconv")),
        "default config must not forward --iconv: {args:?}"
    );
}

#[test]
fn iconv_disabled_omits_iconv_arg() {
    let config = ClientConfig::builder()
        .iconv(IconvSetting::Disabled)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");
    assert!(
        !args
            .iter()
            .any(|a| a.to_string_lossy().starts_with("--iconv")),
        "Disabled iconv must not forward --iconv: {args:?}"
    );
}

#[test]
fn iconv_locale_default_forwards_dot() {
    let config = ClientConfig::builder()
        .iconv(IconvSetting::LocaleDefault)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");
    assert!(
        args.iter().any(|a| a == "--iconv=."),
        "LocaleDefault must forward --iconv=.: {args:?}"
    );
}

#[test]
fn iconv_explicit_pair_forwards_only_remote_half() {
    // upstream: options.c:2717-2721 - `set = strchr(iconv_opt, ','); if (set)
    // set++;` so only the post-comma half (the remote charset) reaches the
    // server. The local charset stays on the client side.
    let config = ClientConfig::builder()
        .iconv(IconvSetting::Explicit {
            local: "UTF-8".to_owned(),
            remote: Some("ISO-8859-1".to_owned()),
        })
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");
    assert!(
        args.iter().any(|a| a == "--iconv=ISO-8859-1"),
        "Explicit pair must forward only the remote half: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--iconv=UTF-8,ISO-8859-1"),
        "Explicit pair must not forward the swapped pair: {args:?}"
    );
}

#[test]
fn iconv_explicit_single_forwards_whole_spec() {
    // upstream: options.c:2718-2721 - `else set = iconv_opt;` so when there
    // is no comma the entire spec is forwarded as the remote charset.
    let config = ClientConfig::builder()
        .iconv(IconvSetting::Explicit {
            local: "UTF-8".to_owned(),
            remote: None,
        })
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");
    assert!(
        args.iter().any(|a| a == "--iconv=UTF-8"),
        "Explicit single must forward the whole spec: {args:?}"
    );
}
