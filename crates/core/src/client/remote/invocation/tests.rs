//! Tests for remote invocation builder and transfer role detection.

use std::ffi::{OsStr, OsString};
use std::time::SystemTime;

use compress::algorithm::CompressionAlgorithm;
use transfer::setup::build_capability_string_suffix;

use super::builder::RemoteInvocationBuilder;
use super::transfer_role::{determine_transfer_role, operand_is_remote};
use super::{RemoteOperands, RemoteRole, TransferSpec};
use crate::client::config::{ClientConfig, IconvSetting, TransferTimeout};

#[test]
fn builds_receiver_invocation_with_sender_flag() {
    // Pull: local is receiver -> remote needs --sender (upstream options.c:2616).
    // upstream: options.c:2728 - capability string is embedded in the compact
    // flag string, producing one argument like `-re.LsfxCIvu`.
    // The local Receiver does not advertise 'i' because oc-rsync's receiver
    // path strips CF_INC_RECURSE from compat_flags (lib.rs::compute_allow_inc_recurse).
    // Advertising 'i' would cause the remote sender to emit NDX_FLIST_EOF that
    // the receiver never consumes, leaving 0xFF bytes that trip read_varint
    // overflow on the next decode.
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");

    assert_eq!(args[0], "rsync");
    assert_eq!(args[1], "--server");
    assert_eq!(args[2], "--sender");
    let flags = args[3].to_string_lossy();
    assert!(flags.starts_with('-'), "flags should start with -: {flags}");
    let expected_suffix = build_capability_string_suffix(false);
    assert!(
        flags.contains(&expected_suffix),
        "capability suffix '{expected_suffix}' must be embedded in flag string: {flags}"
    );
    assert_eq!(args[4], ".");
    assert_eq!(args[5], "/remote/path");
}

#[test]
fn builds_sender_invocation_no_sender_flag() {
    // Push: local is sender -> remote is receiver, no --sender flag.
    // upstream: options.c:2728 - capability string is embedded in the compact
    // flag string, producing one argument like `-re.iLsfxCIvu`.
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    assert_eq!(args[0], "rsync");
    assert_eq!(args[1], "--server");
    // No --sender flag for push - flags come next.
    let flags = args[2].to_string_lossy();
    assert!(flags.starts_with('-'), "flags should start with -: {flags}");
    let expected_suffix = build_capability_string_suffix(true);
    assert!(
        flags.contains(&expected_suffix),
        "capability suffix '{expected_suffix}' must be embedded in flag string: {flags}"
    );
    assert_eq!(args[3], ".");
    assert_eq!(args[4], "/remote/path");
}

#[test]
fn ssh_sender_includes_inc_recurse_capability_by_default() {
    // ISI.h: sender-side INC_RECURSE is default-on, matching upstream rsync
    // 3.4.x. SSH push transfers include the 'i' capability bit by default.
    // upstream: the capability string is embedded in the compact flag string.
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    // Capability string is now embedded in the compact flag string, not separate.
    let flag_str = args[2].to_string_lossy();
    assert!(
        flag_str.contains("e."),
        "flag string must contain embedded capability: {flag_str}"
    );
    // Extract the capability portion after "e."
    let caps_portion = flag_str.split("e.").nth(1).expect("e. separator");
    assert!(
        caps_portion.contains('i'),
        "default sender capability string must include 'i': {flag_str}"
    );
}

#[test]
fn ssh_sender_omits_inc_recurse_when_no_inc_recursive_set() {
    // `--no-inc-recursive` clears `allow_inc_recurse`; the capability bit
    // is suppressed in both transfer directions. Tracker #1862.
    // upstream: capability string is embedded in the compact flag string.
    let config = ClientConfig::builder().inc_recursive_send(false).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    let flag_str = args[2].to_string_lossy();
    assert!(
        flag_str.contains("e."),
        "flag string must contain embedded capability: {flag_str}"
    );
    let caps_portion = flag_str.split("e.").nth(1).expect("e. separator");
    assert!(
        !caps_portion.contains('i'),
        "--no-inc-recursive must suppress 'i' on sender capability: {flag_str}"
    );
}

#[test]
fn ssh_receiver_omits_inc_recurse_capability_by_default() {
    // The local Receiver never advertises 'i' because its receive path
    // strips CF_INC_RECURSE from compat_flags (lib.rs::compute_allow_inc_recurse).
    // Advertising it would cause the remote sender to write the file list
    // in INC_RECURSE format (trailing NDX_FLIST_EOF), the receiver would
    // skip `receive_extra_file_lists`, and the leftover 0xFF byte would
    // trip read_varint overflow on the next decode.
    //
    // upstream: compat.c:162-181 set_allow_inc_recurse() ties 'i' to the
    // local side's actual ability to honor CF_INC_RECURSE.
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");

    // Pull: args[3] is the flag string (after --server, --sender)
    let flag_str = args[3].to_string_lossy();
    assert!(
        flag_str.contains("e."),
        "flag string must contain embedded capability: {flag_str}"
    );
    let caps_portion = flag_str.split("e.").nth(1).expect("e. separator");
    assert!(
        !caps_portion.contains('i'),
        "receiver capability string must omit 'i' to avoid INC_RECURSE wire desync: {flag_str}"
    );
}

#[test]
fn ssh_receiver_omits_inc_recurse_when_no_inc_recursive_set() {
    // `--no-inc-recursive` applies to both directions, matching upstream's
    // single `allow_inc_recurse` global.
    // upstream: capability string is embedded in the compact flag string.
    let config = ClientConfig::builder().inc_recursive_send(false).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");

    // Pull: args[3] is the flag string (after --server, --sender)
    let flag_str = args[3].to_string_lossy();
    assert!(
        flag_str.contains("e."),
        "flag string must contain embedded capability: {flag_str}"
    );
    let caps_portion = flag_str.split("e.").nth(1).expect("e. separator");
    assert!(
        !caps_portion.contains('i'),
        "--no-inc-recursive must suppress 'i' on receiver capability: {flag_str}"
    );
}

#[test]
fn includes_recursive_flag_when_enabled() {
    let config = ClientConfig::builder().recursive(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    // Push layout: rsync --server -flags . /path
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

    let flags = args[2].to_string_lossy();
    assert!(flags.contains('z'), "expected 'z' in flags: {flags}");
}

#[test]
fn includes_log_format_for_itemize() {
    // upstream: options.c:2345-2358,2772-2775 - `-i` alone installs the default
    // "%i %n%L" format, so `stdout_format_has_i` is set and the server arg is
    // --log-format=%i. The CLI models this by setting `out_format_forwards_i`.
    let config = ClientConfig::builder()
        .itemize_changes(true)
        .out_format_forwards_i(true)
        .build();
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
    // Itemize must NOT appear as a transfer-flag character in the compact
    // flag string. The `i` in the capability suffix (`e.iLsfxCIvu`) is the
    // INC_RECURSE bit, not the itemize flag - so we check only the transfer
    // portion (before `e.`).
    let flags = args[2].to_string_lossy();
    let transfer_portion = transfer_flags_portion(&flags);
    assert!(
        !transfer_portion.contains('i'),
        "itemize should not be a compact transfer flag: {flags}"
    );
}

#[test]
fn includes_ii_log_format_for_itemize_unchanged() {
    // upstream: options.c:164-175 server_options - `-ii` forwards
    // `--log-format=%i%I` so the remote generator also itemizes unchanged rows.
    let config = ClientConfig::builder()
        .itemize_changes(true)
        .itemize_unchanged(true)
        .out_format_forwards_i(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    let args_str: Vec<_> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        args_str.contains(&"--log-format=%i%I".to_string()),
        "expected --log-format=%i%I in args: {args_str:?}"
    );
    assert!(
        !args_str.contains(&"--log-format=%i".to_string()),
        "the -i form must not also appear: {args_str:?}"
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

    assert!(
        !args.iter().any(|a| a == "--ignore-errors"),
        "unexpected --ignore-errors in args: {args:?}"
    );
}

// WHY: upstream options.c:2930-2931 emits `--fsync` inside the `if (am_sender)`
// block, so it rides only on a PUSH (RemoteRole::Sender) where the remote
// receiver fsyncs the files it writes. On a PULL the remote sender writes no
// destination files, so forwarding --fsync would be meaningless (and upstream
// never does), while the local receiver still fsyncs its own writes.
#[test]
fn fsync_forwarded_on_push_only() {
    let config = ClientConfig::builder().fsync(true).build();

    let push = RemoteInvocationBuilder::new(&config, RemoteRole::Sender).build("/path");
    assert!(
        push.iter().any(|a| a == "--fsync"),
        "push must forward --fsync: {push:?}"
    );

    let pull = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver).build("/path");
    assert!(
        !pull.iter().any(|a| a == "--fsync"),
        "pull must not forward --fsync to the remote sender: {pull:?}"
    );
}

#[test]
fn omits_fsync_flag_when_not_set() {
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

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
    "--checksum-seed",
    "--partial",
    "--specials",
    "--no-specials",
    "--size-only",
    "--ignore-existing",
    "--existing",
    "--ignore-missing-args",
    "--delete-missing-args",
    "--remove-source-files",
    "--no-implied-dirs",
    "--list-only",
    "--msgs2stderr",
    "--no-msgs2stderr",
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
    "--stop-at",
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
    // upstream: options.c:2655-2657 - copy_links ('L') is a receiver-branch
    // compact letter: it is forwarded to the remote only when the remote is the
    // sender (a pull), so the builder must run in the Receiver role to emit it.
    let config = ClientConfig::builder().copy_links(true).build();
    let flags = receiver_flag_string(&config);
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

/// upstream: options.c:2692 - 'E' is only sent when preserve_perms is false.
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

    // Count 'v' only in the transfer-flag portion, excluding the embedded
    // capability suffix (which contains its own 'v' in e.g. `e.iLsfxCIvu`).
    let flags = args[2].to_string_lossy();
    let transfer_portion = transfer_flags_portion(&flags);
    let v_count = transfer_portion.chars().filter(|c| *c == 'v').count();
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
    let string_args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    // upstream: options.c:2648-2649 - `make_backups` rides in the compact
    // flag string as `b`, NOT as a standalone `--backup` long arg.
    let flag_string = find_flag_string(&string_args);
    assert!(
        transfer_flags_portion(flag_string).contains('b'),
        "expected 'b' in transfer flags portion: {flag_string}"
    );
    assert!(
        !args.iter().any(|a| a == "--backup"),
        "must not emit --backup as a long arg (upstream emits 'b' in flag \
         string instead, options.c:2648-2649): {args:?}"
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

// upstream: options.c:2911-2934 - basis-dir args (--link-dest/--copy-dest/
// --compare-dest) live inside the `if (am_sender)` block, so they are forwarded
// only on a PUSH. On a PULL (RemoteRole::Receiver) the local receiver applies
// them locally and must NOT forward them, or the remote sender would link_stat()
// the flag as a source path. oc previously forwarded them unconditionally.
#[test]
fn reference_directories_not_forwarded_on_pull() {
    let config = ClientConfig::builder()
        .link_destination("/prev")
        .copy_destination("/cpd")
        .compare_destination("/cmp")
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/path");

    assert!(
        !args.iter().any(|a| {
            let s = a.to_string_lossy();
            s.starts_with("--link-dest")
                || s.starts_with("--copy-dest")
                || s.starts_with("--compare-dest")
        }),
        "pull must not forward basis-dir args: {args:?}"
    );
}

// upstream: options.c:2852-2853 - only `--super` (am_root > 1) is forwarded,
// solely on a push. `--fake-super` (am_root == -1) is receiver-local and is
// never forwarded in either direction. Covered by
// `fake_super_not_forwarded_on_pull` / `fake_super_not_forwarded_on_push`.

#[test]
fn forwards_write_devices_to_remote_receiver_only() {
    // upstream: options.c:2979 - `if (write_devices && am_sender) args[ac++] =
    // "--write-devices"`. am_sender is a PUSH, so the flag reaches the remote
    // receiver (RemoteRole::Sender) but never a remote sender (a PULL).
    let config = ClientConfig::builder().write_devices(true).build();

    let push = RemoteInvocationBuilder::new(&config, RemoteRole::Sender).build("/path");
    assert!(
        push.iter().any(|a| a == "--write-devices"),
        "expected --write-devices forwarded on a push: {push:?}"
    );

    let pull = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver).build("/path");
    assert!(
        !pull.iter().any(|a| a == "--write-devices"),
        "did not expect --write-devices forwarded on a pull: {pull:?}"
    );
}

#[test]
fn forwards_copy_devices_to_remote_sender_only() {
    // upstream: options.c:2987 - `if (copy_devices && !am_sender) args[ac++] =
    // "--copy-devices"`. !am_sender is a PULL, so the flag reaches the remote
    // sender (RemoteRole::Receiver) but never a remote receiver (a PUSH).
    let config = ClientConfig::builder().copy_devices(true).build();

    let pull = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver).build("/path");
    assert!(
        pull.iter().any(|a| a == "--copy-devices"),
        "expected --copy-devices forwarded on a pull: {pull:?}"
    );

    let push = RemoteInvocationBuilder::new(&config, RemoteRole::Sender).build("/path");
    assert!(
        !push.iter().any(|a| a == "--copy-devices"),
        "did not expect --copy-devices forwarded on a push: {push:?}"
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
fn forwards_deprecated_remove_sent_files_spelling() {
    // upstream: options.c:2982-2985 - the deprecated `--remove-sent-files`
    // spelling is forwarded verbatim; the canonical form must not also appear.
    let config = ClientConfig::builder()
        .remove_source_files(true)
        .remove_sent_files(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        args.iter().any(|a| a == "--remove-sent-files"),
        "expected --remove-sent-files in args: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--remove-source-files"),
        "canonical spelling must not also appear: {args:?}"
    );
}

#[test]
fn forwards_log_format_o_when_out_format_has_operation() {
    // upstream: options.c:2776-2777 - an out-format with `%o` (and no `%i`)
    // forwards `--log-format=%o` so the remote emits operation output.
    let config = ClientConfig::builder()
        .out_format_has_operation(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args: Vec<_> = builder
        .build("/path")
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(
        args.contains(&"--log-format=%o".to_string()),
        "expected --log-format=%o in args: {args:?}"
    );
}

#[test]
fn omits_log_format_o_on_pull() {
    // upstream: options.c:2768 - the whole chain is gated on `am_sender`; a pull
    // (RemoteRole::Receiver) never forwards a --log-format arg.
    let config = ClientConfig::builder()
        .out_format_has_operation(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args: Vec<_> = builder
        .build("/path")
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(
        !args.iter().any(|a| a.starts_with("--log-format")),
        "pull must not forward --log-format: {args:?}"
    );
}

#[test]
fn forwards_log_format_placeholder_when_not_verbose() {
    // upstream: options.c:2778-2779 - an out-format with neither `%i` nor `%o`
    // forwards the placeholder `--log-format=X` for a non-verbose client.
    let config = ClientConfig::builder().out_format_placeholder(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args: Vec<_> = builder
        .build("/path")
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(
        args.contains(&"--log-format=X".to_string()),
        "expected --log-format=X in args: {args:?}"
    );
}

#[test]
fn omits_log_format_placeholder_when_verbose() {
    // upstream: options.c:2778 - the `X` placeholder is only forwarded when the
    // client is not verbose (`else if (!verbose)`).
    let config = ClientConfig::builder()
        .out_format_placeholder(true)
        .verbosity(1)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args: Vec<_> = builder
        .build("/path")
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(
        !args.contains(&"--log-format=X".to_string()),
        "verbose client must not forward --log-format=X: {args:?}"
    );
}

#[test]
fn out_format_without_i_forwards_o_not_i_even_with_dash_i() {
    // upstream: options.c:2345-2358 - `stdout_format_has_i` is derived from the
    // resolved out-format string, not the `-i` flag. `--out-format="%o" -i`
    // leaves the explicit "%o" format in place (no `%i`), so has_i stays 0 and
    // the server arg must be `--log-format=%o`, NOT `%i`. The CLI models this by
    // NOT setting `out_format_forwards_i` when the explicit format lacks `%i`.
    let config = ClientConfig::builder()
        .itemize_changes(true)
        .out_format_forwards_i(false)
        .out_format_has_operation(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args: Vec<_> = builder
        .build("/path")
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(
        args.contains(&"--log-format=%o".to_string()),
        "explicit %o out-format must forward --log-format=%o: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a.starts_with("--log-format=%i")),
        "must not forward --log-format=%i when the format lacks %i: {args:?}"
    );
}

#[test]
fn explicit_out_format_with_i_forwards_log_format_i() {
    // upstream: options.c:2345-2349 - an explicit `--out-format="%i"` sets
    // `stdout_format_has_i` even without `-i`, so the server arg is
    // --log-format=%i. The CLI models this by setting `out_format_forwards_i`.
    let config = ClientConfig::builder().out_format_forwards_i(true).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args: Vec<_> = builder
        .build("/path")
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(
        args.contains(&"--log-format=%i".to_string()),
        "explicit %i out-format must forward --log-format=%i: {args:?}"
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
    // upstream: options.c:2976 - `--no-implied-dirs` is forwarded only when
    // relative paths are active, so the disabled case must also enable
    // relative paths to reproduce upstream's emission.
    let config = ClientConfig::builder()
        .relative_paths(true)
        .implied_dirs(false)
        .build();
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
fn omits_no_implied_dirs_when_disabled_without_relative_paths() {
    // upstream: options.c:2207-2208 forces `implied_dirs = 0` when relative
    // paths are off, and options.c:2976 gates the `--no-implied-dirs`
    // forwarding on `relative_paths`. A non-relative transfer must therefore
    // never forward `--no-implied-dirs`; otherwise the remote sender
    // link_stat()s the flag as a source path and fails with exit 23.
    let config = ClientConfig::builder().implied_dirs(false).build();
    assert!(!config.relative_paths());
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/path");

    assert!(
        !args.iter().any(|a| a == "--no-implied-dirs"),
        "unexpected --no-implied-dirs in non-relative args: {args:?}"
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

    assert!(
        secluded.stdin_args.is_empty(),
        "stdin_args should be empty when protect_args is off"
    );
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

    let cmd_strs: Vec<String> = secluded
        .command_line_args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(cmd_strs.contains(&"rsync".to_owned()));
    assert!(cmd_strs.contains(&"--server".to_owned()));
    assert!(cmd_strs.contains(&"-s".to_owned()));
    assert!(cmd_strs.contains(&".".to_owned()));

    assert!(
        !cmd_strs.contains(&"/path/to/files".to_owned()),
        "command line should not contain remote path in secluded mode"
    );

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
/// The flag string starts with `-` but not `--`. Since the capability string
/// is now embedded in the flag string (e.g. `-re.iLsfxCIvu`), this returns
/// the combined string. Variable positioning is handled by skipping `--xxx` args.
fn find_flag_string(args: &[String]) -> &str {
    args.iter()
        .find(|a| a.starts_with('-') && !a.starts_with("--"))
        .map(|s| s.as_str())
        .expect("flag string not found in args")
}

/// Helper: extracts the transfer-flag portion of the compact flag string
/// (everything before `e.`), excluding the embedded capability suffix.
///
/// upstream: the compact flag string is `-<transfer-flags>e.<capabilities>`.
/// This returns only the `<transfer-flags>` portion so counting and negative
/// assertions are not affected by the capability characters.
fn transfer_flags_portion(flag_string: &str) -> &str {
    // The capability string starts at the first 'e.' after the leading '-'.
    if let Some(pos) = flag_string[1..].find("e.") {
        &flag_string[..pos + 1]
    } else {
        flag_string
    }
}

/// Helper: extracts the transfer-flag portion from a push (Sender) invocation.
///
/// Returns only the transfer flags (before `e.`), excluding the embedded
/// capability suffix, so that per-flag assertions are not affected by
/// capability characters like `v`, `x`, `L`.
fn sender_flag_string(config: &ClientConfig) -> String {
    let args = build_sender_args(config);
    transfer_flags_portion(find_flag_string(&args)).to_owned()
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

/// Helper: extracts the transfer-flag portion from a pull (Receiver) invocation.
///
/// Returns only the transfer flags (before `e.`), excluding the embedded
/// capability suffix.
fn receiver_flag_string(config: &ClientConfig) -> String {
    let args = build_receiver_args(config);
    transfer_flags_portion(find_flag_string(&args)).to_owned()
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
    // upstream: options.c:2662-2666 - 'W' is only sent when explicitly set.
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
    // upstream: options.c:2658-2659 - copy_dirlinks ('k') is a receiver-branch
    // compact letter, emitted only when the remote is the sender (a pull).
    let config = ClientConfig::builder().copy_dirlinks(true).build();
    let flags = receiver_flag_string(&config);
    assert!(flags.contains('k'), "expected 'k' in flags: {flags}");
}

#[test]
fn includes_devices_flag() {
    let config = ClientConfig::builder().devices(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('D'), "expected 'D' in flags: {flags}");
}

// upstream: options.c:2677-2678 - the compact 'D' letter tracks preserve_devices
// ONLY. specials-only sends NO 'D'; specials ride as the long-form --specials
// (options.c:2760-2765). oc previously packed 'D' for specials, diverging.
#[test]
fn specials_only_emits_long_form_specials_not_compact_d() {
    let config = ClientConfig::builder().specials(true).build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('D'),
        "specials-only must not pack compact 'D': {flags}"
    );
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--specials"),
        "specials-only must emit long-form --specials: {args:?}"
    );
}

// upstream: options.c:2677-2678,2760-2765 - devices sets 'D'; when devices are
// preserved but specials are not, --no-specials is sent (never --devices).
#[test]
fn devices_without_specials_emits_d_and_no_specials() {
    let config = ClientConfig::builder().devices(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('D'), "devices must pack 'D': {flags}");
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--no-specials"),
        "devices-without-specials must emit --no-specials: {args:?}"
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
    let args = build_sender_args(&config);
    // Both preserved: -D carries devices, specials is implied, so neither
    // --specials nor --no-specials is sent (upstream sends nothing here).
    assert!(
        !args
            .iter()
            .any(|a| a == "--specials" || a == "--no-specials"),
        "devices+specials must not emit any specials long-form: {args:?}"
    );
}

#[test]
fn includes_atimes_flag() {
    let config = ClientConfig::builder().atimes(1).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('U'), "expected 'U' in flags: {flags}");
}

// upstream: options.c:2681-2685 - `if (preserve_atimes) { 'U'; if
// (preserve_atimes > 1) 'U'; }`. Level 1 emits a single `-U`, level 2 must emit
// the doubled `-UU` so the remote also preserves directory access times.
// WHY: dropping the doubled letter silently downgrades `-UU` to `-U`, so the
// peer skips directory atimes the user explicitly requested.
#[test]
fn atimes_level_one_emits_single_u() {
    let config = ClientConfig::builder().atimes(1).build();
    let flags = sender_flag_string(&config);
    assert_eq!(
        flags.matches('U').count(),
        1,
        "-U (level 1) must emit exactly one 'U': {flags}"
    );
}

#[test]
fn atimes_level_two_emits_doubled_uu() {
    let config = ClientConfig::builder().atimes(2).build();
    let flags = sender_flag_string(&config);
    assert_eq!(
        flags.matches('U').count(),
        2,
        "-UU (level 2) must emit doubled 'UU': {flags}"
    );
}

// upstream: options.c:2698-2704 - `if (preserve_xattrs) { 'X'; if
// (preserve_xattrs > 1) 'X'; }`. WHY: a `-XX` request that collapses to `-X`
// tells the remote to omit xattrs in a fake-super store, diverging from the
// user's explicit level-2 intent.
#[cfg(all(unix, feature = "xattr"))]
#[test]
fn xattrs_level_one_emits_single_x() {
    let config = ClientConfig::builder().xattrs(1).build();
    let flags = sender_flag_string(&config);
    assert_eq!(
        flags.matches('X').count(),
        1,
        "-X (level 1) must emit exactly one 'X': {flags}"
    );
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn xattrs_level_two_emits_doubled_xx() {
    let config = ClientConfig::builder().xattrs(2).build();
    let flags = sender_flag_string(&config);
    assert_eq!(
        flags.matches('X').count(),
        2,
        "-XX (level 2) must emit doubled 'XX': {flags}"
    );
}

// upstream: options.c:2709-2710 - `if (cvs_exclude) argstr[x++] = 'C';`. The
// letter is forwarded unconditionally (outside the am_sender block) so the
// remote peer runs get_cvs_excludes() itself. WHY: without the letter, an
// upstream peer never activates its own CVS-ignore handling ($HOME/.cvsignore,
// $CVSIGNORE), diverging from a real `rsync -C` invocation.
#[test]
fn cvs_exclude_forwards_compact_c_letter() {
    let config = ClientConfig::builder().cvs_exclude(true).build();
    let flags = sender_flag_string(&config);
    assert!(
        flags.contains('C'),
        "-C must forward the compact 'C' letter: {flags}"
    );
}

#[test]
fn cvs_exclude_absent_by_default() {
    let config = ClientConfig::builder().build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('C'),
        "default config must not emit the 'C' letter: {flags}"
    );
}

// upstream: options.c:2858-2860 - `else { if (skip_compress)
// safe_arg("--skip-compress", skip_compress); }`. Emitted only in the
// `!am_sender` (PULL) branch so the remote sender skips the same suffixes.
// WHY: an explicit `--skip-compress` list that is not forwarded makes the
// remote sender re-compress already-compressed data, wasting CPU and diverging
// from upstream's wire output.
#[test]
fn skip_compress_forwarded_on_pull_when_set() {
    let config = ClientConfig::builder()
        .skip_compress_spec(Some("gz/zip/mp4".to_owned()))
        .build();
    let args = build_receiver_args(&config);
    assert!(
        args.iter().any(|a| a == "--skip-compress=gz/zip/mp4"),
        "explicit --skip-compress must be forwarded on a pull: {args:?}"
    );
}

#[test]
fn skip_compress_not_forwarded_when_default() {
    let config = ClientConfig::builder().build();
    let args = build_receiver_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--skip-compress")),
        "default (built-in) skip-compress list must not be forwarded: {args:?}"
    );
}

#[test]
fn skip_compress_not_forwarded_on_push() {
    // upstream: the `--skip-compress` arg lives in the `else` (!am_sender)
    // branch, so a PUSH (local sender) never forwards it.
    let config = ClientConfig::builder()
        .skip_compress_spec(Some("gz/zip".to_owned()))
        .build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--skip-compress")),
        "--skip-compress must not be forwarded on a push: {args:?}"
    );
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

// upstream: options.c:2511 - server_options() NEVER forwards --trust-sender. It
// only sets internal trust_sender/trust_sender_args locals; the server always
// trusts the client (am_server implies trust). oc previously forwarded it,
// diverging from every upstream server invocation.
#[test]
fn never_forwards_trust_sender_even_when_set() {
    let config = ClientConfig::builder().trust_sender(true).build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a == "--trust-sender"),
        "--trust-sender must never be forwarded to the server: {args:?}"
    );
}

#[test]
fn omits_trust_sender_when_not_set() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a == "--trust-sender"),
        "unexpected --trust-sender in args: {args:?}"
    );
}

#[test]
fn includes_checksum_seed_long_arg() {
    let config = ClientConfig::builder().checksum_seed(Some(12345)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--checksum-seed=12345"),
        "expected --checksum-seed=12345 in args: {args:?}"
    );
}

#[test]
fn omits_checksum_seed_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--checksum-seed=")),
        "unexpected --checksum-seed in args: {args:?}"
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

// upstream: options.c has NO compact 'P' letter. keep_partial rides as the
// long-form --partial, emitted only on a PUSH (am_sender) without --partial-dir
// (options.c:2884-2893). oc previously packed 'P', diverging from upstream.
#[test]
fn partial_emits_long_form_not_compact_p_on_push() {
    let config = ClientConfig::builder().partial(true).build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('P'),
        "compact 'P' must never be packed: {flags}"
    );
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--partial"),
        "push with --partial (no partial-dir) must emit long-form --partial: {args:?}"
    );
}

// upstream: options.c:2884-2893 - the whole partial block is gated on am_sender,
// so a PULL (local is receiver, RemoteRole::Receiver) forwards neither 'P' nor
// --partial; the local receiver keeps partials itself.
#[test]
fn partial_not_forwarded_on_pull() {
    let config = ClientConfig::builder().partial(true).build();
    let flags = receiver_flag_string(&config);
    assert!(!flags.contains('P'), "pull must not pack 'P': {flags}");
    let args = build_receiver_args(&config);
    assert!(
        !args.iter().any(|a| a == "--partial"),
        "pull must not forward --partial: {args:?}"
    );
}

#[test]
fn includes_update_flag() {
    let config = ClientConfig::builder().update(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('u'), "expected 'u' in flags: {flags}");
}

// upstream: options.c:2634-2635 - the compact 'n' letter tracks `!do_xfers`,
// which is set by dry_run ONLY (options.c:2366-2367), never by list_only
// (the "Note: NOT dry_run!" comment). list_only == 1 packs neither 'n' nor
// --list-only. Guards against regressing 'n' back onto the list-only path.
#[test]
fn list_only_does_not_pack_dry_run_n() {
    let config = ClientConfig::builder().list_only(true).build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('n'),
        "list_only must not pack the dry-run 'n' letter: {flags}"
    );
}

// upstream: options.c:2366-2367 - dry_run (and only dry_run) sets do_xfers=0,
// which packs 'n'. The real dry-run path must be preserved.
#[test]
fn dry_run_still_packs_n() {
    let config = ClientConfig::builder().dry_run(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('n'), "dry_run must pack 'n': {flags}");
}

// upstream: options.c:2747-2748 - `if (list_only > 1) "--list-only"`. An
// explicit `--list-only` on a pull forwards the long flag and NEVER packs 'n'
// (list_only does not set do_xfers=0). Sealed argv:
// `-logDtpre.iLsfxCIvu --list-only`.
#[test]
fn list_only_arg_forwards_long_flag_without_n_on_pull() {
    let config = ClientConfig::builder()
        .list_only(true)
        .list_only_arg(true)
        .build();
    let args = build_receiver_args(&config);
    assert!(
        args.iter().any(|a| a == "--list-only"),
        "explicit --list-only must forward the long flag: {args:?}"
    );
    let flags = receiver_flag_string(&config);
    assert!(
        !flags.contains('n'),
        "list-only must not pack the dry-run 'n' letter: {flags}"
    );
}

// upstream: options.c:2747 - `list_only > 1`. The IMPLICIT single-source
// listing (list_only == 1, `list_only_arg` false) is never forwarded.
#[test]
fn implicit_list_only_does_not_forward_long_flag() {
    let config = ClientConfig::builder().list_only(true).build();
    let args = build_receiver_args(&config);
    assert!(
        !args.iter().any(|a| a == "--list-only"),
        "implicit list-only must not forward --list-only: {args:?}"
    );
}

// upstream: options.c:2852-2853 - only `--super` (am_root > 1) is forwarded,
// and only on a push (am_sender). `--fake-super` (am_root == -1) is a
// receiver-local storage mode and is NEVER forwarded in either direction.
#[test]
fn fake_super_not_forwarded_on_pull() {
    let config = ClientConfig::builder().fake_super(true).build();
    let args = build_receiver_args(&config);
    assert!(
        !args.iter().any(|a| a == "--fake-super"),
        "--fake-super must never be forwarded to a remote sender (pull): {args:?}"
    );
}

#[test]
fn fake_super_not_forwarded_on_push() {
    let config = ClientConfig::builder().fake_super(true).build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a == "--fake-super"),
        "--fake-super must never be forwarded to a remote receiver (push): {args:?}"
    );
}

// upstream: options.c:2646-2647 - `if (quiet && msgs2stderr) 'q'`. Default
// msgs2stderr is 2 (nonzero), so plain `-q` packs 'q'. Sealed argv:
// `-qe.LsfxCIvu --msgs2stderr` (with --msgs2stderr) / `-q...` (plain quiet).
#[test]
fn quiet_with_default_msgs2stderr_packs_q() {
    let config = ClientConfig::builder().quiet(true).build();
    let flags = sender_flag_string(&config);
    assert!(flags.contains('q'), "quiet must pack 'q': {flags}");
}

// upstream: options.c:2628 - `--no-msgs2stderr` (msgs2stderr == 0) suppresses
// the 'q' letter even when quiet is set.
#[test]
fn quiet_with_no_msgs2stderr_does_not_pack_q() {
    let config = ClientConfig::builder()
        .quiet(true)
        .msgs2stderr(Some(false))
        .build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('q'),
        "quiet + --no-msgs2stderr must not pack 'q': {flags}"
    );
}

// upstream: options.c:2782-2785 - `--msgs2stderr` (== 1) forwarded long-form.
#[test]
fn msgs2stderr_forwarded_long_form() {
    let config = ClientConfig::builder().msgs2stderr(Some(true)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--msgs2stderr"),
        "--msgs2stderr must be forwarded: {args:?}"
    );
    assert!(!args.iter().any(|a| a == "--no-msgs2stderr"));
}

// upstream: options.c:2784-2785 - `--no-msgs2stderr` (== 0) forwarded long-form.
#[test]
fn no_msgs2stderr_forwarded_long_form() {
    let config = ClientConfig::builder().msgs2stderr(Some(false)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--no-msgs2stderr"),
        "--no-msgs2stderr must be forwarded: {args:?}"
    );
    assert!(!args.iter().any(|a| a == "--msgs2stderr"));
}

// upstream: options.c:2628,2782 - the default (no -q, msgs2stderr == 2) packs
// neither 'q' nor any msgs2stderr long flag. Guards the `-a`-matches seal.
#[test]
fn default_config_no_q_no_msgs2stderr() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args
            .iter()
            .any(|a| a == "--msgs2stderr" || a == "--no-msgs2stderr")
    );
    let flags = sender_flag_string(&config);
    assert!(!flags.contains('q'), "default must not pack 'q': {flags}");
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
    let config = ClientConfig::builder().xattrs(1).build();
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
fn includes_max_alloc_long_arg() {
    let config = ClientConfig::builder()
        .max_alloc(Some(512 * 1024 * 1024))
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--max-alloc=536870912"),
        "expected --max-alloc=536870912 in args: {args:?}"
    );
}

#[test]
fn omits_max_alloc_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--max-alloc=")),
        "should not emit --max-alloc= when none: {args:?}"
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
fn negative_modify_window_uses_short_at_spelling() {
    // WHY: upstream options.c:2874 forwards a negative modify_window via the
    // short `-@%d` spelling (`-@-1`), NOT `--modify-window=-1`, so a stock
    // upstream `--server` receiver honours nanosecond-exact comparison. The
    // long form would be rejected as an invalid unsigned value on the peer.
    let config = ClientConfig::builder().modify_window(Some(-1)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "-@-1"),
        "expected -@-1 in sender args for a negative window: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a.starts_with("--modify-window=")),
        "negative window must not use the long spelling: {args:?}"
    );
}

#[test]
fn modify_window_not_forwarded_on_pull() {
    // WHY: upstream options.c:2873 gates the forwarded arg on `am_sender`. On a
    // pull the local client is the receiver and runs the mtime quick-check
    // itself, so nothing is sent to the remote sender. Forwarding it would
    // diverge from upstream's argv byte-for-byte.
    let config = ClientConfig::builder().modify_window(Some(2)).build();
    let args = build_receiver_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--modify-window=")),
        "pull must not forward --modify-window: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a.starts_with("-@")),
        "pull must not forward -@ window: {args:?}"
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
    // upstream: options.c:2820 - explicit zlib sent as --old-compress
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
    // upstream: options.c:2818 - zlibx sent as --new-compress
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
    // Plain --append (append_mode == 1) emits exactly one --append and never
    // --append-verify. upstream: options.c:2951-2954 server_options().
    let config = ClientConfig::builder().append(true).build();
    let args = build_sender_args(&config);
    let count = args.iter().filter(|a| *a == "--append").count();
    assert_eq!(
        count, 1,
        "plain --append should emit one --append: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--append-verify"),
        "must not forward --append-verify to the server: {args:?}"
    );
}

#[test]
fn append_verify_emits_doubled_append() {
    // --append-verify (append_mode == 2) is encoded on the wire as two bare
    // --append flags, never --append-verify. The server's OPT_APPEND increments
    // append_mode on am_server, so the second flag is what selects verify mode.
    // upstream: options.c:2951-2954 server_options() + options.c:1722-1726.
    let config = ClientConfig::builder().append_verify(true).build();
    let args = build_sender_args(&config);
    let count = args.iter().filter(|a| *a == "--append").count();
    assert_eq!(
        count, 2,
        "append_verify should emit two --append flags: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--append-verify"),
        "must not forward --append-verify to the server: {args:?}"
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

/// upstream: options.c:2711-2712 - `--ignore-times` is emitted as the compact
/// `I` letter in the flag string, never as a long-form `--ignore-times` arg.
/// The long form leaks onto the remote server's positional path list
/// (`link_stat "--ignore-times" failed`), so the compact letter is required.
#[test]
fn ignore_times_rides_compact_flag_not_long_arg() {
    let config = ClientConfig::builder().ignore_times(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('I')),
        "expected 'I' in the compact flag string: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--ignore-times"),
        "must not emit long-form --ignore-times: {args:?}"
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
fn includes_omit_dir_times_compact_flag() {
    // upstream: options.c:2646-2647 - omit_dir_times rides the compact flag
    // string as 'O' inside the am_sender block, not as a standalone long arg.
    let config = ClientConfig::builder().omit_dir_times(true).build();
    let args = build_sender_args(&config);
    let flags = transfer_flags_portion(find_flag_string(&args));
    assert!(
        flags.contains('O'),
        "expected 'O' (omit-dir-times) in compact flags {flags:?}: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--omit-dir-times"),
        "omit-dir-times must not appear as a long arg: {args:?}"
    );
}

#[test]
fn includes_omit_link_times_compact_flag() {
    // upstream: options.c:2648-2649 - omit_link_times rides the compact flag
    // string as 'J' inside the am_sender block, not as a standalone long arg.
    let config = ClientConfig::builder().omit_link_times(true).build();
    let args = build_sender_args(&config);
    let flags = transfer_flags_portion(find_flag_string(&args));
    assert!(
        flags.contains('J'),
        "expected 'J' (omit-link-times) in compact flags {flags:?}: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--omit-link-times"),
        "omit-link-times must not appear as a long arg: {args:?}"
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
fn capability_string_embedded_in_sender_flag_string() {
    // upstream: options.c:2728 - capability suffix is embedded in the compact
    // flag string, not sent as a separate argument.
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    let expected_suffix = build_capability_string_suffix(true);
    let flag_str = find_flag_string(&args);
    assert!(
        flag_str.ends_with(&expected_suffix),
        "expected capability suffix '{expected_suffix}' embedded in flag string '{flag_str}'"
    );
    // Must NOT appear as a standalone `-e.xxx` argument.
    let standalone = format!("-{expected_suffix}");
    assert!(
        !args.iter().any(|a| a == &standalone),
        "capability string must not be a separate argument: {args:?}"
    );
}

#[test]
fn capability_string_embedded_in_receiver_flag_string() {
    // upstream: options.c:2728 - capability suffix is embedded in the compact
    // flag string, not sent as a separate argument.
    // The local Receiver omits 'i' from its advertised capability because
    // its receive path strips CF_INC_RECURSE from compat_flags. See
    // builds_receiver_invocation_with_sender_flag for the rationale.
    let config = ClientConfig::builder().build();
    let args = build_receiver_args(&config);
    let expected_suffix = build_capability_string_suffix(false);
    let flag_str = find_flag_string(&args);
    assert!(
        flag_str.ends_with(&expected_suffix),
        "expected capability suffix '{expected_suffix}' embedded in flag string '{flag_str}'"
    );
    let standalone = format!("-{expected_suffix}");
    assert!(
        !args.iter().any(|a| a == &standalone),
        "capability string must not be a separate argument: {args:?}"
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

// upstream: options.c:2884-2893 - `if (partial_dir && am_sender) { --partial-dir
// ... } else if (keep_partial && am_sender) --partial`. With --partial-dir set,
// the else-if is not taken: --partial-dir is emitted and bare --partial is NOT,
// and there is never a compact 'P'.
#[test]
fn partial_dir_emits_partial_dir_not_compact_p_nor_bare_partial() {
    let config = ClientConfig::builder()
        .partial_directory(Some(".rsync-partial"))
        .build();
    let flags = sender_flag_string(&config);
    assert!(
        !flags.contains('P'),
        "partial-dir must not pack compact 'P': {flags}"
    );
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--partial-dir=.rsync-partial"),
        "expected --partial-dir=.rsync-partial: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--partial"),
        "bare --partial must not be emitted when --partial-dir is set: {args:?}"
    );
}

#[test]
fn backup_without_dir_or_suffix_emits_only_b_short_flag() {
    let config = ClientConfig::builder().backup(true).build();
    let args = build_sender_args(&config);
    // upstream: options.c:2648-2649 - bare `--backup` is `b` in the compact
    // flag string, not a standalone long arg.
    let flag_string = find_flag_string(&args);
    assert!(
        transfer_flags_portion(flag_string).contains('b'),
        "expected 'b' in transfer flags portion: {flag_string}"
    );
    assert!(!args.iter().any(|a| a == "--backup"));
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
    let flag_string = find_flag_string(&args);
    assert!(
        !transfer_flags_portion(flag_string).contains('b'),
        "should not contain 'b' in transfer flags when disabled: {flag_string}"
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
        .atimes(1)
        .permissions(true)
        .executability(true)
        .recursive(true)
        .compress(true)
        .checksum(true)
        .hard_links(true)
        .acls(true)
        .xattrs(1)
        .numeric_ids(true)
        .trust_sender(true)
        .checksum_seed(Some(42))
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
        .stop_at(Some(
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_893_456_000),
        ))
        .set_rsync_path("/custom/rsync")
        .inc_recursive_send(true)
        .build();

    let args = build_sender_args(&config);

    // Structural layout: program name, --server, flags, capability, dot, path
    assert_eq!(args[0], "/custom/rsync");
    assert_eq!(args[1], "--server");

    assert!(args.contains(&"--ignore-errors".to_owned()));
    assert!(args.contains(&"--fsync".to_owned()));

    let full_flags = find_flag_string(&args);
    let flags = transfer_flags_portion(full_flags);
    // upstream: options.c:2655-2659 - copy_links ('L') and copy_dirlinks ('k')
    // are receiver-branch compact letters, so a Sender (push) invocation like
    // this one does NOT emit them. keep_dirlinks ('K') is sender-branch and is
    // present.
    for (ch, name) in [
        ('l', "links"),
        ('K', "keep_dirlinks"),
        ('o', "owner"),
        ('g', "group"),
        ('D', "devices/specials"),
        ('t', "times"),
        ('U', "atimes"),
        ('p', "permissions"),
        // upstream: options.c:2692 - 'E' is only sent when preserve_perms
        // is false (else-if), so it is absent when 'p' is also set.
        ('r', "recursive"),
        ('z', "compress"),
        ('c', "checksum"),
        // upstream: options.c:2711-2712 - ignore_times rides as compact 'I'.
        ('I', "ignore_times"),
        ('H', "hard_links"),
        ('n', "dry_run"),
        ('W', "whole_file"),
        ('S', "sparse"),
        ('y', "fuzzy"),
        ('R', "relative_paths"),
        // upstream: options.c has no compact 'P' letter; --partial rides
        // long-form (and here --partial-dir is set, so neither is emitted).
        ('b', "backup"),
        ('u', "update"),
        ('N', "crtimes"),
        ('m', "prune_empty_dirs"),
        // upstream: options.c:2646-2649 - omit_dir_times ('O') and
        // omit_link_times ('J') ride in the compact flag string inside the
        // am_sender block, never as standalone long args.
        ('O', "omit_dir_times"),
        ('J', "omit_link_times"),
    ] {
        assert!(
            flags.contains(ch),
            "all-flags test: missing '{ch}' ({name}) in flags: {flags}"
        );
    }

    let x_count = flags.chars().filter(|c| *c == 'x').count();
    assert_eq!(x_count, 2, "expected 2 'x' flags for one_file_system=2");

    let v_count = flags.chars().filter(|c| *c == 'v').count();
    assert_eq!(v_count, 2, "expected 2 'v' flags for verbosity=2");

    let expected_long_args = [
        "--delete-before",
        "--delete-excluded",
        "--force",
        "--numeric-ids",
        "--checksum-seed=42",
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
        "--ignore-existing",
        "--existing",
        "--remove-source-files",
        "--no-implied-dirs",
        "--delay-updates",
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

    // upstream: options.c:2825,2852-2853 - the super flag is forwarded only in
    // the am_sender branch (to a remote receiver), so a remote-sender (pull)
    // invocation never carries --fake-super even with fake_super(true).
    assert!(
        !args.iter().any(|a| a == "--fake-super"),
        "all-flags test: --fake-super must not be forwarded to a remote sender: {args:?}"
    );

    // upstream: options.c:2987 - `if (copy_devices && !am_sender)`. This is a
    // push (client is the sender), so --copy-devices is a pull-only flag and
    // must not appear even with copy_devices(true).
    assert!(
        !args.iter().any(|a| a == "--copy-devices"),
        "all-flags test: --copy-devices must not be forwarded on a push: {args:?}"
    );

    // upstream: options.c:2820 - explicit zlib is sent as --old-compress
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
        "--stop-at=",
    ];
    for prefix in expected_prefixed {
        assert!(
            args.iter().any(|a| a.starts_with(prefix)),
            "all-flags test: missing arg with prefix {prefix} in args: {args:?}"
        );
    }

    // upstream: options.c:2728 - capability suffix is embedded in flag string.
    let expected_suffix = build_capability_string_suffix(true);
    let flag_str = find_flag_string(&args);
    assert!(
        flag_str.ends_with(&expected_suffix),
        "all-flags test: capability suffix '{expected_suffix}' must be in flag string '{flag_str}'"
    );
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
// upstream: options.c:2734-2741 - the post-comma half of iconv_opt is
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
    // upstream: options.c:2735-2739 - `set = strchr(iconv_opt, ','); if (set)
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
    // upstream: options.c:2736-2739 - `else set = iconv_opt;` so when there
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

// --- Shell-escaping of filename arguments (safe_arg) ---
// upstream: options.c:safe_arg(NULL, path) backslash-escapes SHELL_CHARS in
// filename args when protect_args is off. These tests verify that oc-rsync
// produces the same escaping, matching the lsh.sh `eval "$@"` contract.

use super::builder::{shell_safe_filename_arg, shell_safe_filename_arg_with_tilde};

#[test]
fn shell_safe_simple_path_unchanged() {
    assert_eq!(shell_safe_filename_arg("/simple/path"), "/simple/path");
}

#[test]
fn shell_safe_escapes_parentheses() {
    // upstream: SHELL_CHARS includes '(' and ')'
    assert_eq!(
        shell_safe_filename_arg("/dir/A weird)name/file"),
        "/dir/A\\ weird\\)name/file"
    );
}

#[test]
fn shell_safe_escapes_spaces() {
    assert_eq!(
        shell_safe_filename_arg("/dir/has space/file"),
        "/dir/has\\ space/file"
    );
}

#[test]
fn shell_safe_escapes_shell_metacharacters() {
    assert_eq!(shell_safe_filename_arg("a&b"), "a\\&b");
    assert_eq!(shell_safe_filename_arg("a;b"), "a\\;b");
    assert_eq!(shell_safe_filename_arg("a|b"), "a\\|b");
    assert_eq!(shell_safe_filename_arg("a<b"), "a\\<b");
    assert_eq!(shell_safe_filename_arg("a>b"), "a\\>b");
    assert_eq!(shell_safe_filename_arg("a{b}"), "a\\{b\\}");
    assert_eq!(shell_safe_filename_arg("a\"b"), "a\\\"b");
    assert_eq!(shell_safe_filename_arg("a'b"), "a\\'b");
    assert_eq!(shell_safe_filename_arg("a`b"), "a\\`b");
    assert_eq!(shell_safe_filename_arg("a#b"), "a\\#b");
    assert_eq!(shell_safe_filename_arg("a$b"), "a\\$b");
    assert_eq!(shell_safe_filename_arg("a!b"), "a\\!b");
}

#[test]
fn shell_safe_escapes_tab() {
    assert_eq!(shell_safe_filename_arg("a\tb"), "a\\\tb");
}

#[test]
fn shell_safe_leading_dash_gets_dot_slash() {
    // upstream: safe_arg prepends "./" to prevent option interpretation
    assert_eq!(shell_safe_filename_arg("-file"), "./-file");
}

#[test]
fn shell_safe_leading_tilde_unescaped_when_not_requested() {
    // The plain wrapper - and a push, where escape_leading_tilde is false -
    // leaves a leading ~ untouched, matching upstream which lets the remote
    // expand ~ on the destination.
    assert_eq!(shell_safe_filename_arg("~foo"), "~foo");
    assert_eq!(shell_safe_filename_arg_with_tilde("~foo", false), "~foo");
}

#[test]
fn shell_safe_leading_tilde_escaped_when_requested() {
    // upstream: options.c:2553-2558 / :2581 - on a pull the leading ~ of a
    // bare-name source path is backslash-escaped to \~foo so the remote shell
    // does not tilde-expand a path literally named ~foo.
    assert_eq!(shell_safe_filename_arg_with_tilde("~foo", true), "\\~foo");
}

#[test]
fn shell_safe_tilde_escape_only_affects_leading_tilde() {
    // A ~ that is not the first character is ordinary (not a SHELL_CHARS
    // member) and is left as-is even when tilde escaping is requested.
    assert_eq!(shell_safe_filename_arg_with_tilde("a~b", true), "a~b");
}

#[test]
fn shell_safe_backslash_not_before_wildcard_is_escaped() {
    // upstream: backslash is escaped unless followed by a wildcard char
    assert_eq!(shell_safe_filename_arg("a\\b"), "a\\\\b");
}

#[test]
fn shell_safe_backslash_before_wildcard_is_preserved() {
    // upstream: backslash before wildcard chars (*?[]) is NOT escaped
    assert_eq!(shell_safe_filename_arg("a\\*b"), "a\\*b");
    assert_eq!(shell_safe_filename_arg("a\\?b"), "a\\?b");
    assert_eq!(shell_safe_filename_arg("a\\[b"), "a\\[b");
    assert_eq!(shell_safe_filename_arg("a\\]b"), "a\\]b");
}

#[test]
fn shell_safe_no_escaping_in_secluded_mode() {
    // When protect_args is active, stdin_args should NOT be shell-escaped
    let config = ClientConfig::builder().protect_args(Some(true)).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let secluded = builder.build_secluded(&["/path/A weird)name/"]);

    assert!(
        secluded
            .stdin_args
            .iter()
            .any(|a| a == "/path/A weird)name/"),
        "secluded stdin_args should contain unescaped path: {:?}",
        secluded.stdin_args
    );
}

#[test]
fn shell_safe_escaping_in_normal_mode() {
    // When protect_args is off, command_line_args should have escaped paths
    let config = ClientConfig::builder().protect_args(None).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let secluded = builder.build_secluded(&["/path/A weird)name/"]);

    assert!(
        secluded
            .command_line_args
            .iter()
            .any(|a| a.to_string_lossy() == "/path/A\\ weird\\)name/"),
        "normal command_line_args should contain escaped path: {:?}",
        secluded.command_line_args
    );
}

// --usermap / --groupmap forwarding tests

#[cfg(unix)]
#[test]
fn includes_groupmap_wildcard_verbatim() {
    // upstream: options.c:2916 - --groupmap is forwarded verbatim under
    // `protect_args` (the default). The wildcard `*` must survive so the
    // receiver's `uidlist.c:parse_name_map()` installs a wildcard rule.
    let mapping = ::metadata::GroupMapping::parse("*:1234").expect("parse");
    let config = ClientConfig::builder().group_mapping(Some(mapping)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--groupmap=*:1234"),
        "expected --groupmap=*:1234 verbatim: {args:?}"
    );
}

#[cfg(unix)]
#[test]
fn includes_usermap_wildcard_verbatim() {
    let mapping = ::metadata::UserMapping::parse("*:5678").expect("parse");
    let config = ClientConfig::builder().user_mapping(Some(mapping)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--usermap=*:5678"),
        "expected --usermap=*:5678 verbatim: {args:?}"
    );
}

#[test]
fn omits_usermap_and_groupmap_when_unset() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--usermap")),
        "default config must not emit --usermap: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a.starts_with("--groupmap")),
        "default config must not emit --groupmap: {args:?}"
    );
}

#[cfg(unix)]
#[test]
fn forwards_multi_rule_groupmap_verbatim() {
    let mapping = ::metadata::GroupMapping::parse("100-200:1234,wheel:9999,*:0").expect("parse");
    let config = ClientConfig::builder().group_mapping(Some(mapping)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter()
            .any(|a| a == "--groupmap=100-200:1234,wheel:9999,*:0"),
        "expected multi-rule groupmap verbatim (no rule reordering): {args:?}"
    );
}

// --stop-at forwarding tests

#[test]
fn includes_stop_at_long_arg_when_set() {
    use std::time::{Duration, SystemTime};

    let deadline = SystemTime::UNIX_EPOCH + Duration::from_secs(1_893_456_000);
    let config = ClientConfig::builder().stop_at(Some(deadline)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a.starts_with("--stop-at=")),
        "expected --stop-at=... in args: {args:?}"
    );
}

#[test]
fn omits_stop_at_when_none() {
    let config = ClientConfig::builder().build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a.starts_with("--stop-at=")),
        "should not emit --stop-at= when none: {args:?}"
    );
}

#[test]
fn stop_at_forwarded_as_utc_datetime_format() {
    use std::time::{Duration, SystemTime};

    // 2030-01-15T12:30:00 UTC = 1_894_796_200 unix seconds
    // (2030-01-15 12:30:00 UTC)
    let deadline = SystemTime::UNIX_EPOCH + Duration::from_secs(1_894_796_200);
    let config = ClientConfig::builder().stop_at(Some(deadline)).build();
    let args = build_sender_args(&config);
    let stop_arg = args
        .iter()
        .find(|a| a.starts_with("--stop-at="))
        .expect("--stop-at arg");
    // The formatted value should be a valid datetime like YYYY/MM/DDTHH:MM
    let value = stop_arg.strip_prefix("--stop-at=").unwrap();
    assert!(
        value.contains('T'),
        "stop-at value should contain 'T' separator: {value}"
    );
    assert!(
        value.contains('/') || value.contains('-'),
        "stop-at value should contain date separators: {value}"
    );
}

#[test]
fn stop_at_forwarded_in_secluded_mode() {
    use std::time::{Duration, SystemTime};

    let deadline = SystemTime::UNIX_EPOCH + Duration::from_secs(1_893_456_000);
    let config = ClientConfig::builder()
        .stop_at(Some(deadline))
        .protect_args(Some(true))
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let secluded = builder.build_secluded(&["/path"]);

    assert!(
        secluded
            .stdin_args
            .iter()
            .any(|a| a.starts_with("--stop-at=")),
        "secluded stdin_args should contain --stop-at=: {:?}",
        secluded.stdin_args
    );
}

// format_system_time_for_stop_at and unix_secs_to_utc_components tests

#[test]
fn format_stop_at_unix_epoch() {
    use super::builder::format_system_time_for_stop_at;

    let formatted = format_system_time_for_stop_at(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(formatted, "1970/01/01T00:00");
}

#[test]
fn format_stop_at_y2k() {
    use super::builder::format_system_time_for_stop_at;
    use std::time::Duration;

    // 2000-01-01T00:00:00 UTC = 946_684_800 (well-known Y2K timestamp)
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(946_684_800);
    let formatted = format_system_time_for_stop_at(time).unwrap();
    assert_eq!(formatted, "2000/01/01T00:00");
}

#[test]
fn format_stop_at_round_trip_with_parser() {
    use super::builder::format_system_time_for_stop_at;
    use std::time::Duration;

    // Verify the formatter output is parseable by checking the format structure.
    // Use a future timestamp to ensure it's always valid.
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(4_102_444_800);
    let formatted = format_system_time_for_stop_at(time).unwrap();
    // Must be YYYY/MM/DDTHH:MM
    let parts: Vec<&str> = formatted.split('T').collect();
    assert_eq!(parts.len(), 2, "must have date and time parts: {formatted}");
    let date_parts: Vec<&str> = parts[0].split('/').collect();
    assert_eq!(date_parts.len(), 3, "date must have 3 parts: {}", parts[0]);
    let time_parts: Vec<&str> = parts[1].split(':').collect();
    assert_eq!(time_parts.len(), 2, "time must have 2 parts: {}", parts[1]);
}

#[test]
fn unix_secs_to_utc_epoch() {
    use super::builder::unix_secs_to_utc_components;
    let (y, m, d, h, min) = unix_secs_to_utc_components(0);
    assert_eq!((y, m, d, h, min), (1970, 1, 1, 0, 0));
}

#[test]
fn unix_secs_to_utc_known_date() {
    use super::builder::unix_secs_to_utc_components;
    // 2000-01-01T00:00:00 UTC = 946_684_800
    let (y, m, d, h, min) = unix_secs_to_utc_components(946_684_800);
    assert_eq!((y, m, d, h, min), (2000, 1, 1, 0, 0));
}

#[test]
fn unix_secs_to_utc_one_day() {
    use super::builder::unix_secs_to_utc_components;
    // 1970-01-02T00:00:00 UTC = 86400
    let (y, m, d, h, min) = unix_secs_to_utc_components(86_400);
    assert_eq!((y, m, d, h, min), (1970, 1, 2, 0, 0));
}

#[test]
fn unix_secs_to_utc_time_components() {
    use super::builder::unix_secs_to_utc_components;
    // 1970-01-01T23:59:00 UTC = 86340
    let (y, m, d, h, min) = unix_secs_to_utc_components(86_340);
    assert_eq!((y, m, d, h, min), (1970, 1, 1, 23, 59));
}

#[test]
fn unix_secs_to_utc_y2k() {
    use super::builder::unix_secs_to_utc_components;
    // 2000-01-01T00:00:00 UTC = 946_684_800 (well-known Y2K timestamp)
    let (y, m, d, h, min) = unix_secs_to_utc_components(946_684_800);
    assert_eq!((y, m, d, h, min), (2000, 1, 1, 0, 0));
}

// Remote option (-M / --remote-option) forwarding

#[test]
fn remote_options_appended_to_sender_invocation() {
    // upstream: options.c:3004-3011 - remote_options[] appended after all
    // other server args, before "." and remote paths.
    let config = ClientConfig::builder()
        .remote_options(vec!["--bwlimit=100", "--compress-level=1"])
        .build();
    let args = build_sender_args(&config);

    assert!(
        args.iter().any(|a| a == "--bwlimit=100"),
        "expected --bwlimit=100 from -M in args: {args:?}"
    );
    assert!(
        args.iter().any(|a| a == "--compress-level=1"),
        "expected --compress-level=1 from -M in args: {args:?}"
    );
}

#[test]
fn remote_options_appended_to_receiver_invocation() {
    let config = ClientConfig::builder()
        .remote_options(vec!["--timeout=60"])
        .build();
    let args = build_receiver_args(&config);

    assert!(
        args.iter().any(|a| a == "--timeout=60"),
        "expected --timeout=60 from -M in args: {args:?}"
    );
}

#[test]
fn remote_options_appear_before_dot_placeholder() {
    // upstream: server_options() appends remote_options before returning,
    // then do_cmd() appends "." and paths. So remote options must precede ".".
    let config = ClientConfig::builder()
        .remote_options(vec!["--bwlimit=200"])
        .build();
    let args = build_sender_args(&config);

    let dot_idx = args.iter().position(|a| a == ".").unwrap();
    let opt_idx = args.iter().position(|a| a == "--bwlimit=200").unwrap();
    assert!(
        opt_idx < dot_idx,
        "remote option should appear before '.' placeholder: {args:?}"
    );
}

#[test]
fn remote_options_appear_after_locally_derived_args() {
    // Remote options should come after all locally-derived long-form args
    // but before "." and paths.
    let config = ClientConfig::builder()
        .numeric_ids(true)
        .remote_options(vec!["--max-delete=50"])
        .build();
    let args = build_sender_args(&config);

    let numeric_idx = args.iter().position(|a| a == "--numeric-ids").unwrap();
    let remote_idx = args.iter().position(|a| a == "--max-delete=50").unwrap();
    assert!(
        remote_idx > numeric_idx,
        "remote option should appear after locally-derived args: {args:?}"
    );
}

#[test]
fn empty_remote_options_adds_nothing() {
    let config_with = ClientConfig::builder()
        .remote_options(Vec::<&str>::new())
        .build();
    let config_without = ClientConfig::builder().build();
    let args_with = build_sender_args(&config_with);
    let args_without = build_sender_args(&config_without);

    assert_eq!(
        args_with, args_without,
        "empty remote_options should produce identical invocation"
    );
}

#[test]
fn multiple_remote_options_preserve_order() {
    let config = ClientConfig::builder()
        .remote_options(vec!["--first", "--second", "--third"])
        .build();
    let args = build_sender_args(&config);

    let first_idx = args.iter().position(|a| a == "--first").unwrap();
    let second_idx = args.iter().position(|a| a == "--second").unwrap();
    let third_idx = args.iter().position(|a| a == "--third").unwrap();
    assert!(
        first_idx < second_idx && second_idx < third_idx,
        "remote options must preserve insertion order: {args:?}"
    );
}

#[test]
fn remote_options_included_in_secluded_stdin_args() {
    // When protect_args is active, remote options must appear in the
    // stdin_args (the full argument list), not on the SSH command line.
    let config = ClientConfig::builder()
        .protect_args(Some(true))
        .remote_options(vec!["--bwlimit=500"])
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let secluded = builder.build_secluded(&["/data"]);

    assert!(
        secluded.stdin_args.iter().any(|a| a == "--bwlimit=500"),
        "secluded stdin_args should contain remote option: {:?}",
        secluded.stdin_args
    );
    // The minimal SSH command line should NOT contain the remote option.
    assert!(
        !secluded
            .command_line_args
            .iter()
            .any(|a| a.to_string_lossy() == "--bwlimit=500"),
        "secluded command_line_args should NOT contain remote option: {:?}",
        secluded.command_line_args
    );
}

#[test]
fn remote_option_short_flag_forwarded_verbatim() {
    // -M can forward short flags like -v or compound options.
    let config = ClientConfig::builder().remote_options(vec!["-v"]).build();
    let args = build_sender_args(&config);

    assert!(
        args.iter().any(|a| a == "-v"),
        "expected short flag -v from -M in args: {args:?}"
    );
}

/// SSH push with a local `--files-from` must NOT forward the option to the
/// remote receiver. Upstream's `options.c:2962` gate
/// `if (files_from && (!am_sender || filesfrom_host))` skips emission when
/// the client is the sender and the list lives locally. The local sender
/// reads the file directly to build the file list; emitting
/// `--files-from=-` would make the remote receiver wait for entries that
/// the sender's main loop never forwards.
#[test]
fn push_with_local_files_from_omits_remote_arg() {
    use crate::client::config::FilesFromSource;
    use std::path::PathBuf;

    let config = ClientConfig::builder()
        .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
        .build();
    // RemoteRole::Sender == local is sender (push).
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/dest");

    assert!(
        !args.iter().any(|a| {
            let s = a.to_string_lossy();
            s.starts_with("--files-from")
        }),
        "PUSH with local files-from must not send --files-from to the remote: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--from0"),
        "PUSH with local files-from must not send --from0 to the remote: {args:?}"
    );
}

/// SSH push with `--files-from=-` (stdin) is treated identically to a local
/// file source: the local sender consumes stdin to build the list and the
/// remote receiver gets no `--files-from` arg.
#[test]
fn push_with_stdin_files_from_omits_remote_arg() {
    use crate::client::config::FilesFromSource;

    let config = ClientConfig::builder()
        .files_from(FilesFromSource::Stdin)
        .from0(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/dest");

    assert!(
        !args.iter().any(|a| {
            let s = a.to_string_lossy();
            s.starts_with("--files-from")
        }),
        "PUSH with stdin files-from must not send --files-from to the remote: {args:?}"
    );
}

/// SSH push with a remote-hosted `--files-from` (`host:path` or `:path`)
/// forwards the path to the remote receiver, which opens the file and
/// forwards its bytes back over the wire via `start_filesfrom_forwarding`.
#[test]
fn push_with_remote_files_from_forwards_path() {
    use crate::client::config::FilesFromSource;

    let config = ClientConfig::builder()
        .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/dest");

    assert!(
        args.iter()
            .any(|a| a.to_string_lossy() == "--files-from=/remote/list.txt"),
        "PUSH with remote files-from must forward the path to the remote: {args:?}"
    );
}

/// SSH pull with a local `--files-from` must forward `--files-from=- --from0`
/// to the remote sender, which reads filenames from the wire. The receiver
/// (us) forwards the file's bytes after sending the filter list.
#[test]
fn pull_with_local_files_from_sends_files_from_stdin_to_remote() {
    use crate::client::config::FilesFromSource;
    use std::path::PathBuf;

    let config = ClientConfig::builder()
        .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
        .build();
    // RemoteRole::Receiver == local is receiver (pull).
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/source");

    assert!(
        args.iter().any(|a| a == "--files-from=-"),
        "PULL with local files-from must forward --files-from=- to the remote: {args:?}"
    );
    assert!(
        args.iter().any(|a| a == "--from0"),
        "PULL with local files-from must forward --from0 to the remote: {args:?}"
    );
}

/// upstream: options.c:368-369 - a PULL with `--files-from --no-relative`
/// (relative_paths resolved off) must forward `--no-relative` to the remote
/// sender AND must NOT pack the compact `R` letter. Without this the remote
/// defaults relative_paths=1 (options.c:2205-2206) and keeps the leading path
/// components (`sub/file` instead of the flattened `file`).
#[test]
fn pull_with_files_from_no_relative_forwards_no_relative_and_omits_r() {
    use crate::client::config::FilesFromSource;
    use std::path::PathBuf;

    let config = ClientConfig::builder()
        .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
        .relative_paths(false)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/source");

    assert!(
        args.iter().any(|a| a == "--no-relative"),
        "files-from + non-relative must forward --no-relative: {args:?}"
    );
    let flags = receiver_flag_string(&config);
    assert!(
        !flags.contains('R'),
        "files-from + non-relative must NOT pack compact 'R': {flags}"
    );
}

/// upstream: options.c:109-110 - a PULL with `--files-from` defaulting to
/// relative (relative_paths resolved on) packs the compact `R` letter and does
/// NOT forward `--no-relative`.
#[test]
fn pull_with_files_from_relative_packs_r_and_omits_no_relative() {
    use crate::client::config::FilesFromSource;
    use std::path::PathBuf;

    let config = ClientConfig::builder()
        .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
        .relative_paths(true)
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/source");

    assert!(
        !args.iter().any(|a| a == "--no-relative"),
        "relative files-from must NOT forward --no-relative: {args:?}"
    );
    let flags = receiver_flag_string(&config);
    assert!(
        flags.contains('R'),
        "relative files-from must pack compact 'R': {flags}"
    );
}

/// SSH pull with a remote-hosted `--files-from` forwards the absolute path
/// (matching upstream `options.c:2964 safe_arg("", files_from)`).
#[test]
fn pull_with_remote_files_from_forwards_path() {
    use crate::client::config::FilesFromSource;

    let config = ClientConfig::builder()
        .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
        .build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/source");

    assert!(
        args.iter()
            .any(|a| a.to_string_lossy() == "--files-from=/remote/list.txt"),
        "PULL with remote files-from must forward the path to the remote: {args:?}"
    );
}

// WHY: `--use-qsort` must reach the peer so both sides sort file lists with
// the same comparator; upstream forwards it unconditionally (options.c:2908).
#[test]
fn ssh_forwards_use_qsort_when_set() {
    let config = ClientConfig::builder().qsort(true).build();
    for role in [RemoteRole::Sender, RemoteRole::Receiver] {
        let builder = RemoteInvocationBuilder::new(&config, role);
        let args = builder.build("/remote/path");
        assert!(
            args.iter().any(|a| a == "--use-qsort"),
            "qsort must forward --use-qsort ({role:?}): {args:?}"
        );
    }
    let off = ClientConfig::builder().build();
    let args = RemoteInvocationBuilder::new(&off, RemoteRole::Sender).build("/remote/path");
    assert!(!args.iter().any(|a| a == "--use-qsort"));
}

// WHY: `--super` (explicit, upstream am_root > 1) is forwarded only on a push,
// where the remote receiver performs the privileged operations
// (options.c:2852, inside the am_sender block).
#[test]
fn ssh_forwards_super_on_push_only() {
    let config = ClientConfig::builder().super_user(true).build();
    let push = RemoteInvocationBuilder::new(&config, RemoteRole::Sender).build("/remote/path");
    assert!(
        push.iter().any(|a| a == "--super"),
        "push must forward --super: {push:?}"
    );
    let pull = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver).build("/remote/path");
    assert!(
        !pull.iter().any(|a| a == "--super"),
        "pull must not forward --super: {pull:?}"
    );
    // Not requested: never forwarded.
    let off = ClientConfig::builder().build();
    let args = RemoteInvocationBuilder::new(&off, RemoteRole::Sender).build("/remote/path");
    assert!(!args.iter().any(|a| a == "--super"));
}

// WHY: `--stats` is forwarded only on a push, where the remote
// receiver/generator computes the transfer statistics (options.c:2856, inside
// the am_sender block).
#[test]
fn ssh_forwards_stats_on_push_only() {
    let config = ClientConfig::builder().stats(true).build();
    let push = RemoteInvocationBuilder::new(&config, RemoteRole::Sender).build("/remote/path");
    assert!(
        push.iter().any(|a| a == "--stats"),
        "push must forward --stats: {push:?}"
    );
    let pull = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver).build("/remote/path");
    assert!(
        !pull.iter().any(|a| a == "--stats"),
        "pull must not forward --stats: {pull:?}"
    );
    let off = ClientConfig::builder().build();
    let args = RemoteInvocationBuilder::new(&off, RemoteRole::Sender).build("/remote/path");
    assert!(!args.iter().any(|a| a == "--stats"));
}

// WHY: explicitly-set --info / --debug levels must reach the peer so its
// diagnostic output matches the user's request (upstream make_output_option,
// options.c:2947). `del` is receiver-side, so on a push it forwards as
// `--info=del`; `send` is sender-side, so on a pull it forwards as
// `--debug=send`.
#[test]
fn ssh_forwards_info_and_debug_when_set() {
    let config = ClientConfig::builder()
        .info_flags([OsString::from("del1")])
        .debug_flags([OsString::from("send1")])
        .build();
    let push = RemoteInvocationBuilder::new(&config, RemoteRole::Sender).build("/remote/path");
    assert!(
        push.iter().any(|a| a == "--info=del"),
        "push must forward receiver-side --info=del: {push:?}"
    );
    let pull = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver).build("/remote/path");
    assert!(
        pull.iter().any(|a| a == "--debug=send"),
        "pull must forward sender-side --debug=send: {pull:?}"
    );

    // Nothing set: no --info / --debug argument at all.
    let off = ClientConfig::builder().build();
    let args = RemoteInvocationBuilder::new(&off, RemoteRole::Sender).build("/remote/path");
    assert!(
        !args
            .iter()
            .any(|a| a.to_string_lossy().starts_with("--info=")
                || a.to_string_lossy().starts_with("--debug=")),
        "no info/debug flags must yield no --info/--debug arg: {args:?}"
    );
}

// WHY (OPT-GAP-01, HIGH DATA-LOSS): upstream options.c:2826-2831 remaps a
// max-delete ceiling of 0 to `--max-delete=-1` before forwarding. The remote
// receiver treats `--max-delete=0` as UNLIMITED (options.c:2182-2184 disables
// the cap for max_delete <= 0), so a client that ran `--max-delete=0 --delete`
// - meaning "delete NOTHING" - would instead delete EVERY extraneous file if we
// forwarded `--max-delete=0` verbatim. The inversion (0 -> -1) is what makes the
// remote enforce the "delete nothing" ceiling the user asked for.
#[test]
fn max_delete_zero_forwards_minus_one_not_zero_on_push() {
    let config = ClientConfig::builder().max_delete(Some(0)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--max-delete=-1"),
        "max-delete=0 must forward as --max-delete=-1 (unlimited-cap inversion): {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--max-delete=0"),
        "max-delete=0 must NEVER reach the remote verbatim (deletes everything): {args:?}"
    );
}

// WHY (OPT-GAP-01): a positive ceiling is forwarded unchanged, matching
// upstream's `if (max_delete > 0) --max-delete=N` branch.
#[test]
fn max_delete_positive_forwarded_verbatim_on_push() {
    let config = ClientConfig::builder().max_delete(Some(7)).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--max-delete=7"),
        "positive max-delete must forward verbatim: {args:?}"
    );
}

// WHY (OPT-GAP-01 + OPT-GAP-05): every option in upstream's `if (am_sender)`
// block (options.c:2825-2857) is receiver-steering, so on a PULL (the local
// process is the receiver, RemoteRole::Receiver) NONE of them may be forwarded
// to the remote sender. In particular --max-delete=0 must not leak even in its
// remapped form, because the remote sender performs no deletion at all.
#[test]
fn sender_only_delete_and_size_options_not_forwarded_on_pull() {
    let config = ClientConfig::builder()
        .max_delete(Some(0))
        .max_file_size(Some(1_048_576))
        .min_file_size(Some(1024))
        .delete_before(true)
        .force_replacements(true)
        .size_only(true)
        .build();
    let pull = build_receiver_args(&config);
    for needle in [
        "--max-delete=-1",
        "--max-delete=0",
        "--max-size=1048576",
        "--min-size=1024",
        "--delete-before",
        "--force",
        "--size-only",
    ] {
        assert!(
            !pull.iter().any(|a| a == needle),
            "PULL must not forward am_sender-only {needle} to the remote sender: {pull:?}"
        );
    }

    // Same config on a PUSH forwards them (the remote receiver needs them).
    let push = build_sender_args(&config);
    assert!(
        push.iter().any(|a| a == "--delete-before") && push.iter().any(|a| a == "--size-only"),
        "PUSH must still forward the am_sender-only options: {push:?}"
    );
}

// WHY (OPT-GAP-05, most-important leak): upstream options.c:2846-2847 emits
// --delete-excluded only inside `if (am_sender)`. On a PULL, forwarding it
// rewrites the remote sender's send_rules so excluded files vanish from the
// file list, corrupting what the receiver sees. It must ride only on a PUSH.
#[test]
fn delete_excluded_forwarded_on_push_only() {
    let config = ClientConfig::builder()
        .delete_excluded(true)
        .delete_before(true)
        .build();

    let push = build_sender_args(&config);
    assert!(
        push.iter().any(|a| a == "--delete-excluded"),
        "PUSH must forward --delete-excluded: {push:?}"
    );

    let pull = build_receiver_args(&config);
    assert!(
        !pull.iter().any(|a| a == "--delete-excluded"),
        "PULL must NOT forward --delete-excluded (it rewrites the remote sender's send_rules): {pull:?}"
    );
}

// WHY (OPT-GAP-05): --usermap / --groupmap, --ignore-existing / --existing,
// --temp-dir and --preallocate all live in upstream's `if (am_sender)` block
// (options.c:2911-2943, 2990). They steer the remote receiver, so a PULL must
// not forward them to the remote sender.
#[cfg(unix)]
#[test]
fn sender_only_mapping_and_dest_options_not_forwarded_on_pull() {
    let user = ::metadata::UserMapping::parse("*:5678").expect("parse");
    let group = ::metadata::GroupMapping::parse("*:1234").expect("parse");
    let config = ClientConfig::builder()
        .user_mapping(Some(user))
        .group_mapping(Some(group))
        .ignore_existing(true)
        .existing_only(true)
        .temp_directory(Some("/tmp/staging"))
        .preallocate(true)
        .build();

    let pull = build_receiver_args(&config);
    for needle in [
        "--usermap=*:5678",
        "--groupmap=*:1234",
        "--ignore-existing",
        "--existing",
        "--temp-dir=/tmp/staging",
        "--preallocate",
    ] {
        assert!(
            !pull.iter().any(|a| a == needle),
            "PULL must not forward am_sender-only {needle}: {pull:?}"
        );
    }

    let push = build_sender_args(&config);
    for needle in [
        "--usermap=*:5678",
        "--groupmap=*:1234",
        "--ignore-existing",
        "--existing",
        "--temp-dir=/tmp/staging",
        "--preallocate",
    ] {
        assert!(
            push.iter().any(|a| a == needle),
            "PUSH must forward am_sender-only {needle}: {push:?}"
        );
    }
}

// WHY (OPT-GAP-02): upstream options.c:2799 forwards `--bwlimit=%d` in whole KiB
// (options.c:1718 `bwlimit = (size + 512) / 1024`), NOT bytes/sec. The remote
// peer re-parses the value with a default `K` suffix (options.c:1714), so a raw
// byte count is scaled up 1024x and the throttle effectively vanishes. A rate of
// 1 MiB/s (1048576 B/s) must travel as `--bwlimit=1024`.
#[test]
fn bwlimit_forwarded_in_kib_not_bytes() {
    use crate::client::config::BandwidthLimit;
    use std::num::NonZeroU64;
    let config = ClientConfig::builder()
        .bandwidth_limit(Some(BandwidthLimit::from_bytes_per_second(
            NonZeroU64::new(1_048_576).unwrap(),
        )))
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--bwlimit=1024"),
        "bwlimit must forward as whole KiB: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--bwlimit=1048576"),
        "bwlimit must NOT forward the raw byte count: {args:?}"
    );
}

// upstream: options.c:2750-2753 - `--no-r` tells the remote receiver that a
// dirs-mode delete (`-d --delete`) is NOT recursive. Without it the receiver
// could re-enable recursion and delete beyond the top level, so the flag is
// load-bearing for delete correctness, not cosmetic.
#[test]
fn dirs_delete_push_forwards_no_r() {
    // `ClientConfig::builder()` presets recursion on, so a `-d --delete` (no
    // `-r`) transfer must explicitly clear it to reach the guard.
    let config = ClientConfig::builder()
        .recursive(false)
        .dirs(true)
        .delete(true)
        .build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--no-r"),
        "expected --no-r for -d --delete push: {args:?}"
    );
}

// upstream: options.c:2752 - the guard requires !recurse; with recursion on the
// receiver already recurses, so --no-r must NOT be sent.
#[test]
fn recursive_delete_push_omits_no_r() {
    let config = ClientConfig::builder()
        .dirs(true)
        .recursive(true)
        .delete(true)
        .build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a == "--no-r"),
        "recursive delete must NOT send --no-r: {args:?}"
    );
}

// upstream: options.c:2752 - the guard requires delete_mode; without --delete
// there is nothing to protect, so --no-r must NOT be sent.
#[test]
fn dirs_push_without_delete_omits_no_r() {
    let config = ClientConfig::builder().recursive(false).dirs(true).build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a == "--no-r"),
        "dirs push without --delete must NOT send --no-r: {args:?}"
    );
}

// upstream: options.c:2752 - the guard requires am_sender; on a PULL the local
// process is the receiver, so --no-r must NOT be forwarded to the remote sender.
#[test]
fn dirs_delete_pull_omits_no_r() {
    let config = ClientConfig::builder()
        .recursive(false)
        .dirs(true)
        .delete(true)
        .build();
    let args = build_receiver_args(&config);
    assert!(
        !args.iter().any(|a| a == "--no-r"),
        "dirs delete PULL must NOT send --no-r: {args:?}"
    );
}

// upstream: options.c:2993 - `if (open_noatime && preserve_atimes <= 1)`. Plain
// --open-noatime (no -U) is below the threshold, so the flag is forwarded.
#[test]
fn open_noatime_forwarded_without_atimes() {
    let config = ClientConfig::builder().open_noatime(true).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--open-noatime"),
        "expected --open-noatime: {args:?}"
    );
}

// upstream: options.c:2993 - a single -U (preserve_atimes == 1) is still <= 1,
// so --open-noatime is forwarded.
#[test]
fn open_noatime_forwarded_with_single_atimes() {
    let config = ClientConfig::builder().open_noatime(true).atimes(1).build();
    let args = build_sender_args(&config);
    assert!(
        args.iter().any(|a| a == "--open-noatime"),
        "expected --open-noatime with -U: {args:?}"
    );
}

// upstream: options.c:2993 - `-UU` (preserve_atimes == 2) exceeds the threshold,
// so --open-noatime is suppressed even though open_noatime is set.
#[test]
fn open_noatime_suppressed_with_double_atimes() {
    let config = ClientConfig::builder().open_noatime(true).atimes(2).build();
    let args = build_sender_args(&config);
    assert!(
        !args.iter().any(|a| a == "--open-noatime"),
        "-UU must suppress --open-noatime: {args:?}"
    );
}
