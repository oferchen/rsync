//! Round-trip guard against server-arg allow-list drift.
//!
//! `is_known_server_long_flag` is a hand-maintained allow-list. When a
//! value-bearing flag the client actually emits is missing from it,
//! `parse_server_flag_string_and_args` cannot tell the flag's operand from a
//! transfer path, so the `--opt=val` (or `-Xval`) token falls through into
//! `positional_args` and becomes a spurious source/destination path - silently
//! corrupting the transfer.
//!
//! This has bitten repeatedly: the `--bwlimit=` / `--block-size` leaks these
//! tests were added for, `-e` (#7153), and earlier #6569 / #6587. Asserting one
//! flag at a time never catches the NEXT drift. Instead, this module builds the
//! real server argv with [`RemoteInvocationBuilder`] for a representative matrix
//! of client configs and both transfer roles, feeds it through the production
//! parser, and asserts that `positional_args` holds ONLY the genuine transfer
//! paths - zero leaked option tokens - while the forwarded values are still
//! captured. Any future emitter/allow-list divergence fails here.
//!
//! (`RemoteInvocationBuilder` lives in the `core` crate; the parser functions
//! are the `pub(super)` production entry points used by `run.rs`.)

use std::ffi::OsString;
use std::num::{NonZeroU32, NonZeroU64};

use core::client::remote::invocation::{RemoteInvocationBuilder, RemoteRole};
use core::client::{BandwidthLimit, ClientConfig, TransferTimeout};

use super::flags::parse_server_long_flags;
use super::parse::parse_server_flag_string_and_args;

/// Builds the server argv exactly as the SSH transport would, then returns the
/// slice the server actually parses: production `run.rs` drops the arg0 program
/// name (`&args[1..]`) before handing the tail to the parser.
fn server_argv(config: &ClientConfig, role: RemoteRole, paths: &[&str]) -> Vec<OsString> {
    let os_paths: Vec<&std::ffi::OsStr> = paths.iter().map(|p| std::ffi::OsStr::new(*p)).collect();
    RemoteInvocationBuilder::new(config, role).build_with_paths(&os_paths)
}

/// Core invariant: after parsing, `positional_args` must equal exactly the
/// transfer paths - no token may start with `-`, which is the signature of an
/// allow-list miss leaking an option into the path list.
fn assert_no_positional_leak(config: &ClientConfig, role: RemoteRole, paths: &[&str]) {
    let built = server_argv(config, role, paths);
    let (flag_string, positional) = parse_server_flag_string_and_args(&built[1..]);

    assert!(
        flag_string.starts_with('-') && !flag_string.starts_with("--"),
        "expected a compact flag string, got {flag_string:?} in {built:?}"
    );

    for token in &positional {
        let text = token.to_string_lossy();
        assert!(
            !text.starts_with('-'),
            "leaked option token `{text}` in positional_args - \
             is_known_server_long_flag drifted from the emitter: {built:?}"
        );
    }

    let got: Vec<String> = positional
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let want: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
    assert_eq!(
        got, want,
        "positional_args must be exactly the transfer paths: {built:?}"
    );
}

#[test]
fn bwlimit_does_not_leak_and_is_captured() {
    // upstream: options.c:2799-2800 - `--bwlimit=%d` (whole KiB), emitted
    // unconditionally (both roles). The parser must recognise the joined form
    // AND capture its value.
    let config = ClientConfig::builder()
        .bandwidth_limit(Some(BandwidthLimit::from_bytes_per_second(
            NonZeroU64::new(4 * 1024 * 1024).unwrap(),
        )))
        .build();

    for role in [RemoteRole::Sender, RemoteRole::Receiver] {
        assert_no_positional_leak(&config, role, &["/remote/path"]);
        let built = server_argv(&config, role, &["/remote/path"]);
        assert!(
            built
                .iter()
                .any(|a| a.to_string_lossy().starts_with("--bwlimit=")),
            "bwlimit must forward on {role:?}: {built:?}"
        );
        let long = parse_server_long_flags(&built[1..]);
        assert!(
            long.bwlimit.is_some(),
            "server parser must capture the forwarded bwlimit on {role:?}: {built:?}"
        );
    }
}

#[test]
fn block_size_emits_short_b_and_does_not_leak() {
    // upstream: options.c:2788 - `-B%u` short spelling. The old non-upstream
    // long `--block-size=` form leaked because is_known only knows `-B<digits>`.
    let config = ClientConfig::builder()
        .block_size_override(Some(NonZeroU32::new(131072).unwrap()))
        .build();

    for role in [RemoteRole::Sender, RemoteRole::Receiver] {
        assert_no_positional_leak(&config, role, &["/remote/path"]);
        let built = server_argv(&config, role, &["/remote/path"]);
        assert!(
            built.iter().any(|a| a == "-B131072"),
            "block_size must forward as upstream `-B131072` on {role:?}: {built:?}"
        );
        assert!(
            !built
                .iter()
                .any(|a| a.to_string_lossy().starts_with("--block-size")),
            "the non-upstream long --block-size spelling must be gone: {built:?}"
        );
        let long = parse_server_long_flags(&built[1..]);
        assert_eq!(
            long.block_size.as_deref(),
            Some("131072"),
            "server parser must capture the -B value on {role:?}: {built:?}"
        );
    }
}

#[test]
fn temp_dir_and_backup_dir_joined_forms_do_not_leak() {
    // upstream emits these SPLIT (options.c:2807-2808 / 2926-2927); oc's own
    // forwarder emits the JOINED `--flag=value`. Both must be recognised. These
    // are am_sender (PUSH) only, so exercise the Sender role.
    let config = ClientConfig::builder()
        .temp_directory(Some("/var/tmp/oc-spill"))
        .backup(true)
        .backup_directory(Some("/srv/backups"))
        .backup_suffix(Some(".orig"))
        .build();

    assert_no_positional_leak(&config, RemoteRole::Sender, &["/remote/dest"]);

    let built = server_argv(&config, RemoteRole::Sender, &["/remote/dest"]);
    assert!(
        built.iter().any(|a| a == "--temp-dir=/var/tmp/oc-spill"),
        "temp-dir must forward joined: {built:?}"
    );
    assert!(
        built.iter().any(|a| a == "--backup-dir=/srv/backups"),
        "backup-dir must forward joined: {built:?}"
    );
}

#[test]
fn broad_flag_mix_never_leaks_positionals() {
    // A single config that lights up the value-bearing, leak-prone flags at once
    // (the boolean flags fold into the compact string and cannot become
    // positionals). `-e.<caps>` is embedded in the compact flag string of every
    // invocation, so the #7153 `-e` class is covered here too.
    let config = ClientConfig::builder()
        .bandwidth_limit(Some(BandwidthLimit::from_bytes_per_second(
            NonZeroU64::new(8 * 1024 * 1024).unwrap(),
        )))
        .block_size_override(Some(NonZeroU32::new(65536).unwrap()))
        .temp_directory(Some("/tmp/oc"))
        .backup(true)
        .backup_directory(Some("/bak"))
        .backup_suffix(Some("~"))
        .compare_destination("/cmp")
        .link_destination("/lnk")
        .copy_destination("/cpy")
        .timeout(TransferTimeout::Seconds(NonZeroU64::new(300).unwrap()))
        .build();

    // Sender (PUSH): dest-refs, temp-dir, backup-dir all emit here. Single path.
    assert_no_positional_leak(&config, RemoteRole::Sender, &["/remote/dest"]);
    // Receiver (PULL): bwlimit/block-size/timeout still emit. Multi-path exercises
    // the `.` separator and back-to-back paths.
    assert_no_positional_leak(&config, RemoteRole::Receiver, &["/remote/a", "/remote/b"]);
}
