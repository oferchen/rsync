use super::super::connection::DaemonTransferRequest;
use super::arguments::build_full_daemon_args;
use super::server_config::{build_server_config_for_generator, build_server_config_for_receiver};
use super::transfer::{
    DaemonProgressAdapter, is_dry_run_remote_close, read_files_from_for_forwarding,
};

use crate::client::config::ClientConfig;
use crate::client::module_list::DaemonAddress;

use protocol::ProtocolVersion;

mod protect_args_daemon_tests {
    use super::super::arguments::build_minimal_daemon_args;
    use super::*;

    fn test_daemon_request() -> DaemonTransferRequest {
        DaemonTransferRequest {
            address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
            module: "test".to_owned(),
            path: String::new(),
            username: None,
        }
    }

    // upstream: clientserver.c:395-402 - phase 1 wire emits sargs[0..NULL].
    // Upstream's phase 1 NEVER contains a standalone `.` (which only enters
    // sargs after server_options() returns, AFTER the NULL marker at
    // options.c:2745). Upstream's phase 1 also never contains a bare `-s`:
    // the `s` is embedded in the compact flag string `argstr` at
    // options.c:2622-2623. We mirror that shape by emitting only the role
    // markers + `--secluded-args` (the long-form alias of `-s` per
    // options.c:804) so the merged daemon arg list does not carry a stray
    // `.` that would short-circuit `apply_long_form_args`' dot_position
    // bound, nor a bare `-s` that would shadow phase 2's real compact flag
    // string in the daemon's first-short-form-arg picker.
    #[test]
    fn build_minimal_args_receiver_excludes_dot_and_bare_short_flag() {
        let args = build_minimal_daemon_args(false);
        assert_eq!(args, vec!["--server", "--secluded-args"]);
        assert!(
            !args.iter().any(|a| a == "."),
            "phase 1 must not emit a standalone `.` - upstream's wire never does",
        );
        assert!(
            !args.iter().any(|a| a == "-s"),
            "phase 1 must not emit a bare `-s` - the daemon's flag-string \
             picker would shadow the real compact flag string from phase 2",
        );
    }

    #[test]
    fn build_minimal_args_sender_excludes_dot_and_bare_short_flag() {
        let args = build_minimal_daemon_args(true);
        assert_eq!(args, vec!["--server", "--sender", "--secluded-args"]);
        assert!(!args.iter().any(|a| a == "."));
        assert!(!args.iter().any(|a| a == "-s"));
    }

    #[test]
    fn build_full_args_includes_module_path() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert_eq!(args[0], "--server");
        assert!(args.contains(&".".to_owned()));
        let last = args.last().unwrap();
        assert!(last.starts_with(&request.module));
    }

    #[test]
    fn build_full_args_sender_flag() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert_eq!(args[0], "--server");
        assert_eq!(args[1], "--sender");
    }

    /// Finds the compact flag string in the daemon args (starts with `-`, not `--`).
    fn find_flag_string(args: &[String]) -> &str {
        args.iter()
            .find(|a| a.starts_with('-') && !a.starts_with("--"))
            .map(|s| s.as_str())
            .expect("flag string not found in daemon args")
    }

    #[test]
    fn build_full_args_capability_flags_protocol30() {
        // upstream: options.c:2710 - capability string is embedded in the
        // compact flag string for protocol >= 30.
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        let flags = find_flag_string(&args);
        assert!(
            flags.contains("e."),
            "protocol 30+ must embed capability in flag string: {flags}"
        );
    }

    #[test]
    fn build_full_args_no_capability_flags_protocol29() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        let flags = find_flag_string(&args);
        assert!(
            !flags.contains("e."),
            "protocol 29 must not embed capability: {flags}"
        );
    }

    #[test]
    fn build_full_args_push_includes_inc_recurse_capability_by_default() {
        // ISI.h: sender-side INC_RECURSE is default-on, matching upstream
        // rsync 3.4.x. The daemon push capability includes 'i' by default.
        // upstream: capability string is embedded in the compact flag string.
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let request = test_daemon_request();

        let config_default = ClientConfig::default();
        let args_default = build_full_daemon_args(&config_default, &request, protocol, false);
        let flags_default = find_flag_string(&args_default);
        let caps_default = flags_default
            .split("e.")
            .nth(1)
            .expect("capability suffix present");
        assert!(
            caps_default.contains('i'),
            "default push capability must include 'i': {flags_default}"
        );

        let config_off = ClientConfig::builder().inc_recursive_send(false).build();
        let args_off = build_full_daemon_args(&config_off, &request, protocol, false);
        let flags_off = find_flag_string(&args_off);
        let caps_off = flags_off
            .split("e.")
            .nth(1)
            .expect("capability suffix present");
        assert!(
            !caps_off.contains('i'),
            "--no-inc-recursive must suppress 'i' on push capability: {flags_off}"
        );
    }

    #[test]
    fn build_full_args_pull_omits_inc_recurse_capability_to_match_receiver_role() {
        // Pull means the daemon is sender; the local client side is the
        // receiver. oc-rsync's receiver path strips CF_INC_RECURSE from
        // compat_flags (lib.rs::compute_allow_inc_recurse) but does not
        // tell the peer. Advertising 'i' would cause the remote sender to
        // write the file list in INC_RECURSE format (trailing NDX_FLIST_EOF),
        // the receiver would skip `receive_extra_file_lists`, and the
        // leftover 0xFF byte would trip read_varint overflow on the next
        // decode.
        //
        // upstream: compat.c:162-181 set_allow_inc_recurse(),
        // options.c:3036 maybe_add_e_option() - 'i' is gated on
        // allow_inc_recurse which reflects the local side's actual ability
        // to honor CF_INC_RECURSE.
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let request = test_daemon_request();

        let config_default = ClientConfig::default();
        let args_default = build_full_daemon_args(&config_default, &request, protocol, true);
        let flags_default = find_flag_string(&args_default);
        let caps_default = flags_default
            .split("e.")
            .nth(1)
            .expect("capability suffix present");
        assert!(
            !caps_default.contains('i'),
            "pull (daemon-is-sender) must omit 'i' to avoid INC_RECURSE wire desync: {flags_default}"
        );

        let config_off = ClientConfig::builder().inc_recursive_send(false).build();
        let args_off = build_full_daemon_args(&config_off, &request, protocol, true);
        let flags_off = find_flag_string(&args_off);
        let caps_off = flags_off
            .split("e.")
            .nth(1)
            .expect("capability suffix present");
        assert!(
            !caps_off.contains('i'),
            "--no-inc-recursive must suppress 'i' on pull capability: {flags_off}"
        );
    }

    // upstream: options.c:2655-2660 - `if (am_sender) { ... } else { if
    // (copy_links) 'L'; if (copy_dirlinks) 'k'; }`. On a PULL the daemon is the
    // sender (is_sender=true, we_are_sender=false), so L/k ride the wire to it -
    // copy_links/copy_dirlinks dereference symlinks on the SENDER. A builder that
    // sent them on a push (to the remote receiver) diverged from upstream.
    #[test]
    fn build_full_args_pull_packs_copy_links_and_dirlinks() {
        let config = ClientConfig::builder()
            .copy_links(true)
            .copy_dirlinks(true)
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, true);

        let flags = find_flag_string(&args);
        // Isolate the transfer letters from the `e.` capability suffix, which
        // itself carries an 'L' (log_format) that would false-positive.
        let transfer = flags.split("e.").next().expect("transfer letters");
        assert!(
            transfer.contains('L'),
            "pull (daemon-is-sender) must pack 'L' (copy-links): {flags}"
        );
        assert!(
            transfer.contains('k'),
            "pull (daemon-is-sender) must pack 'k' (copy-dirlinks): {flags}"
        );
    }

    // upstream: options.c:2641-2654 - on a PUSH the local client is the sender
    // (is_sender=false, we_are_sender=true), so server_options() takes the
    // `am_sender` branch and never packs L/k. The local sender dereferences
    // symlinks itself; the remote receiver must not receive copy-links.
    #[test]
    fn build_full_args_push_omits_copy_links_and_dirlinks() {
        let config = ClientConfig::builder()
            .copy_links(true)
            .copy_dirlinks(true)
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        let flags = find_flag_string(&args);
        let transfer = flags.split("e.").next().expect("transfer letters");
        assert!(
            !transfer.contains('L'),
            "push must omit 'L' (copy-links rides only to a remote sender): {flags}"
        );
        assert!(
            !transfer.contains('k'),
            "push must omit 'k' (copy-dirlinks rides only to a remote sender): {flags}"
        );
    }

    // upstream: options.c:2625-2626 - `for (i = 0; i < verbose; i++)
    // argstr[x++] = 'v';`. The daemon wire must carry one 'v' per verbosity
    // level so the remote half emits matching verbose diagnostics. The `e.`
    // capability suffix also contains a 'v', so the count is checked against
    // the transfer-letter portion only.
    #[test]
    fn build_full_args_packs_verbose_v_per_level() {
        let config = ClientConfig::builder().verbosity(2).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        let flags = find_flag_string(&args);
        let transfer = flags.split("e.").next().expect("transfer letters");
        assert_eq!(
            transfer.matches('v').count(),
            2,
            "verbosity 2 must pack exactly two 'v' in the transfer letters: {flags}"
        );
    }

    #[test]
    fn build_full_args_includes_compare_dest() {
        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--compare-dest=/tmp/compare"),
            "expected --compare-dest=/tmp/compare in args: {args:?}"
        );
    }

    #[test]
    fn build_full_args_includes_copy_dest() {
        let config = ClientConfig::builder()
            .copy_destination("/tmp/copy")
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--copy-dest=/tmp/copy"),
            "expected --copy-dest=/tmp/copy in args: {args:?}"
        );
    }

    #[test]
    fn build_full_args_includes_link_dest() {
        let config = ClientConfig::builder().link_destination("/prev").build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--link-dest=/prev"),
            "expected --link-dest=/prev in args: {args:?}"
        );
    }

    // upstream: options.c:2866-2869 - --delete-missing-args is always
    // forwarded to the daemon because deleting a missing arg needs both
    // sides to cooperate: the sender emits the mode-0 sentinel and the
    // receiver must have missing_args==2 to accept it (flist.c:884) and
    // run delete_item (generator.c:1360). Without this the daemon rejects
    // the mode-0 entry with "invalid file mode 00" (upstream #910) or,
    // against an oc daemon, silently keeps the stale destination path.
    #[test]
    fn build_full_args_forwards_delete_missing_args_on_push() {
        let config = ClientConfig::builder().delete_missing_args(true).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        // is_sender=false => daemon is receiver (client push).
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--delete-missing-args"),
            "push must forward --delete-missing-args so the daemon accepts \
             the mode-0 sentinel and deletes the missing arg: {args:?}"
        );
    }

    #[test]
    fn build_full_args_forwards_delete_missing_args_on_pull() {
        let config = ClientConfig::builder().delete_missing_args(true).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        // is_sender=true => daemon is sender (client pull). Both sides still
        // need the flag, so it is forwarded regardless of direction.
        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            args.iter().any(|a| a == "--delete-missing-args"),
            "pull must also forward --delete-missing-args (both sides cooperate): {args:?}"
        );
    }

    // upstream: options.c:2870-2871 - --ignore-missing-args is forwarded
    // only when the local side is the receiver (`!am_sender`); a sender can
    // honour ignore by itself and never tells the receiver. Here
    // is_sender=true means the daemon is the sender, so the local side is
    // the receiver and the flag must be forwarded.
    #[test]
    fn build_full_args_forwards_ignore_missing_args_only_on_pull() {
        let config = ClientConfig::builder().ignore_missing_args(true).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        // Pull (daemon is sender, local side is receiver): forwarded.
        let pull = build_full_daemon_args(&config, &request, protocol, true);
        assert!(
            pull.iter().any(|a| a == "--ignore-missing-args"),
            "pull (local receiver) must forward --ignore-missing-args: {pull:?}"
        );

        // Push (daemon is receiver, local side is sender): NOT forwarded,
        // the sender handles the missing arg locally.
        let push = build_full_daemon_args(&config, &request, protocol, false);
        assert!(
            !push.iter().any(|a| a == "--ignore-missing-args"),
            "push (local sender) must not forward --ignore-missing-args: {push:?}"
        );
    }

    // Mutual exclusivity: with both flags set the builder emits only the
    // delete form (upstream options.c:2867's `if/else if` gives
    // --delete-missing-args precedence, matching options.c:2236-2237 where
    // missing_args==3 is simplified to 2).
    #[test]
    fn build_full_args_delete_wins_over_ignore_missing_args() {
        let config = ClientConfig::builder()
            .delete_missing_args(true)
            .ignore_missing_args(true)
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(args.iter().any(|a| a == "--delete-missing-args"));
        assert!(
            !args.iter().any(|a| a == "--ignore-missing-args"),
            "delete-missing-args must take precedence: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_missing_args_by_default() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args
                .iter()
                .any(|a| a == "--delete-missing-args" || a == "--ignore-missing-args"),
            "no missing-args flag by default: {args:?}"
        );
    }

    #[test]
    fn build_full_args_includes_multiple_reference_dirs() {
        let config = ClientConfig::builder()
            .link_destination("/prev1")
            .link_destination("/prev2")
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--link-dest=/prev1"),
            "expected --link-dest=/prev1 in args: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--link-dest=/prev2"),
            "expected --link-dest=/prev2 in args: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_reference_dirs_when_empty() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--compare-dest=")
                || a.starts_with("--copy-dest=")
                || a.starts_with("--link-dest=")),
            "should not emit reference dir args when empty: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_reference_dirs_in_pull_mode() {
        // upstream: options.c:2915-2923 - reference dirs are inside if(am_sender).
        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .link_destination("/prev")
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            !args.iter().any(|a| a.starts_with("--compare-dest=")
                || a.starts_with("--copy-dest=")
                || a.starts_with("--link-dest=")),
            "pull mode should not send reference dir args to daemon: {args:?}"
        );
    }

    #[test]
    fn build_full_args_includes_log_format_for_itemize_push() {
        // upstream: options.c:2750-2762 - --log-format=%i sent when am_sender
        // (client is sender / push mode) so daemon receiver emits itemize via MSG_INFO.
        let config = ClientConfig::builder().itemize_changes(true).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        // is_sender=false means daemon is NOT sender, i.e., client IS sender (push)
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--log-format=%i"),
            "push with itemize should include --log-format=%i: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_log_format_for_itemize_pull() {
        // upstream: options.c:2752 - --log-format only sent when am_sender.
        // In pull mode (daemon is sender), client handles itemize locally.
        let config = ClientConfig::builder().itemize_changes(true).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        // is_sender=true means daemon IS sender (pull)
        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            !args.iter().any(|a| a == "--log-format=%i"),
            "pull with itemize should not include --log-format=%i: {args:?}"
        );
    }

    #[test]
    fn build_full_args_uses_ii_log_format_for_itemize_unchanged_push() {
        // upstream: options.c:164-175 server_options - `-ii`
        // (stdout_format_has_i > 1) forwards `--log-format=%i%I` so the daemon
        // receiver also itemizes unchanged entries. `-i` alone forwards `%i`.
        let config = ClientConfig::builder()
            .itemize_changes(true)
            .itemize_unchanged(true)
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--log-format=%i%I"),
            "push with -ii should forward --log-format=%i%I: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--log-format=%i"),
            "the -i form must not also appear: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_log_format_without_itemize() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--log-format")),
            "should not include --log-format without itemize: {args:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_full_args_forwards_groupmap_wildcard_verbatim() {
        // upstream: options.c:2894-2898 - --groupmap value is shipped verbatim
        // through the daemon secluded-args byte stream. Wildcards like `*`
        // must survive so the receiver's `uidlist.c:parse_name_map()` sees
        // `strpbrk(cp, "*[?")` and installs a `NFLAGS_WILD_NAME_MATCH` rule.
        let group_mapping = ::metadata::GroupMapping::parse("*:1234").expect("parse groupmap");
        let config = ClientConfig::builder()
            .group_mapping(Some(group_mapping))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--groupmap=*:1234"),
            "expected --groupmap=*:1234 verbatim (no backslash escape): {args:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_full_args_forwards_usermap_wildcard_verbatim() {
        let user_mapping = ::metadata::UserMapping::parse("*:5678").expect("parse usermap");
        let config = ClientConfig::builder()
            .user_mapping(Some(user_mapping))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            args.iter().any(|a| a == "--usermap=*:5678"),
            "expected --usermap=*:5678 verbatim: {args:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_full_args_forwards_groupmap_multi_rule_verbatim() {
        // Multi-rule specs (comma-separated) must round-trip without rule
        // reordering or whitespace mangling.
        let group_mapping =
            ::metadata::GroupMapping::parse("100-200:1234,wheel:9999,*:0").expect("parse groupmap");
        let config = ClientConfig::builder()
            .group_mapping(Some(group_mapping))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter()
                .any(|a| a == "--groupmap=100-200:1234,wheel:9999,*:0"),
            "expected multi-rule groupmap verbatim: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_groupmap_when_unset() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--groupmap")),
            "default config must not emit --groupmap: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.starts_with("--usermap")),
            "default config must not emit --usermap: {args:?}"
        );
    }

    #[test]
    fn secluded_args_round_trip_preserves_groupmap_wildcard() {
        // Wire-byte regression: the secluded-args (protect-args) protocol must
        // ship `--groupmap=*:1234` byte-for-byte from sender to receiver. This
        // mirrors the wire path used when `protect_args` is active and the
        // daemon reads phase-2 args via `recv_secluded_args()` -- the upstream
        // `read_args()` equivalent in oc-rsync.
        use protocol::secluded_args::{recv_secluded_args, send_secluded_args};
        use std::io::Cursor;

        let sent = vec![
            "rsync",
            "--server",
            "--groupmap=*:1234",
            "--usermap=*:5678",
            ".",
            "module/",
        ];
        let mut wire = Vec::new();
        send_secluded_args(&mut wire, &sent, None).expect("send");

        // Wildcard must appear unescaped on the wire.
        assert!(
            wire.windows(b"--groupmap=*:1234\0".len())
                .any(|w| w == b"--groupmap=*:1234\0"),
            "wildcard '*' must reach the wire unescaped"
        );

        let mut cursor = Cursor::new(wire);
        let received = recv_secluded_args(&mut cursor, None).expect("recv");
        assert_eq!(received, sent);
    }

    // UTS-15.b: the client-side daemon argv builder must never emit
    // `--write-batch` / `--read-batch` / `--only-write-batch`. They are
    // client-local flags (upstream `options.c:784-786`) that drive batch-
    // file tee or replay on the client; routing them to the daemon caused
    // a silent connection close at protocol byte ~2241725 because the
    // daemon popt table rejects them in daemon mode
    // (`options.c:1444-1449`).
    //
    // Today no code path adds them to `build_full_daemon_args`, but the
    // sanitiser at the end of the builder is defense-in-depth: a future
    // refactor that forwards `remote_options` or any other user-supplied
    // list into the daemon path would otherwise reintroduce the leak.
    #[test]
    fn build_full_args_strips_write_batch_flag() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let mut args = build_full_daemon_args(&config, &request, protocol, false);
        // Simulate a defect upstream of the sanitiser: inject the flag.
        args.insert(2, "--write-batch=/tmp/batch.out".to_owned());

        super::super::arguments::tests::strip_for_test(&mut args);

        assert!(
            !args.iter().any(|a| a.starts_with("--write-batch")),
            "--write-batch must not reach the daemon argv: {args:?}"
        );
    }

    #[test]
    fn build_full_args_strips_read_batch_flag_and_orphan_value() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let mut args = build_full_daemon_args(&config, &request, protocol, false);
        // Two-arg form: flag + bare positional value. Both must drop so the
        // value does not become an orphan path argument the daemon would
        // mis-parse as a module-relative source.
        args.insert(2, "--read-batch".to_owned());
        args.insert(3, "/tmp/batch.in".to_owned());

        super::super::arguments::tests::strip_for_test(&mut args);

        assert!(
            !args.iter().any(|a| a == "--read-batch"),
            "--read-batch must not reach the daemon argv: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "/tmp/batch.in"),
            "two-arg value must not orphan-leak: {args:?}"
        );
    }

    #[test]
    fn build_full_args_strips_only_write_batch_flag() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let mut args = build_full_daemon_args(&config, &request, protocol, false);
        args.insert(2, "--only-write-batch=/tmp/dry.out".to_owned());

        super::super::arguments::tests::strip_for_test(&mut args);

        assert!(
            !args.iter().any(|a| a.starts_with("--only-write-batch")),
            "--only-write-batch must not reach the daemon argv: {args:?}"
        );
    }

    #[test]
    fn build_full_args_default_path_emits_no_batch_flags() {
        // upstream: options.c:server_options() never emits write-batch /
        // read-batch tokens; the only related token is the literal
        // `--only-write-batch=X` placeholder at options.c:2832-2833 which
        // upstream only writes when `write_batch < 0`. The default path of
        // our builder must mirror that.
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        for arg in &args {
            assert!(
                !arg.starts_with("--write-batch")
                    && !arg.starts_with("--read-batch")
                    && !arg.starts_with("--only-write-batch"),
                "default daemon argv contained batch flag {arg:?}: {args:?}"
            );
        }
    }
}

mod server_config_reference_dirs {
    use super::*;
    use crate::client::config::ReferenceDirectoryKind;

    #[test]
    fn receiver_config_propagates_reference_directories() {
        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .link_destination("/prev")
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();

        assert_eq!(server_config.reference_directories.len(), 2);
        assert_eq!(
            server_config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Compare
        );
        assert_eq!(
            server_config.reference_directories[0]
                .path()
                .to_str()
                .unwrap(),
            "/tmp/compare"
        );
        assert_eq!(
            server_config.reference_directories[1].kind(),
            ReferenceDirectoryKind::Link
        );
        assert_eq!(
            server_config.reference_directories[1]
                .path()
                .to_str()
                .unwrap(),
            "/prev"
        );
    }

    #[test]
    fn generator_config_propagates_reference_directories() {
        let config = ClientConfig::builder()
            .copy_destination("/tmp/copy")
            .build();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();

        assert_eq!(server_config.reference_directories.len(), 1);
        assert_eq!(
            server_config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Copy
        );
        assert_eq!(
            server_config.reference_directories[0]
                .path()
                .to_str()
                .unwrap(),
            "/tmp/copy"
        );
    }

    #[test]
    fn receiver_config_empty_reference_dirs_by_default() {
        let config = ClientConfig::default();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();

        assert!(server_config.reference_directories.is_empty());
    }

    #[test]
    fn generator_config_empty_reference_dirs_by_default() {
        let config = ClientConfig::default();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();

        assert!(server_config.reference_directories.is_empty());
    }

    #[test]
    fn generator_config_sets_files_from_for_local_file_push() {
        // upstream: options.c:2944 - when the client is the sender and
        // --files-from points to a local file, the generator reads filenames
        // directly from the file (not via the protocol stream).
        let config = ClientConfig::builder()
            .files_from(crate::client::config::FilesFromSource::LocalFile(
                std::path::PathBuf::from("/tmp/list.txt"),
            ))
            .build();

        let local_paths = vec!["src/".to_owned()];
        let server_config =
            build_server_config_for_generator(&config, &local_paths, Vec::new()).unwrap();

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some("/tmp/list.txt"),
            "generator should read files-from from local file for push"
        );
    }

    #[test]
    fn generator_config_sets_files_from_for_remote_source() {
        let config = ClientConfig::builder()
            .files_from(crate::client::config::FilesFromSource::RemoteFile(
                "/srv/list.txt".to_owned(),
            ))
            .build();

        let local_paths = vec!["src/".to_owned()];
        let server_config =
            build_server_config_for_generator(&config, &local_paths, Vec::new()).unwrap();

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some("-"),
            "generator should read files-from from protocol stream for remote source"
        );
        assert!(
            server_config.file_selection.from0,
            "remote files-from uses NUL-separated wire format"
        );
    }

    #[test]
    fn generator_config_propagates_itemize_flag() {
        // upstream: options.c:2750-2762 - the local ServerConfig must have
        // info_flags.itemize set so the generator's maybe_emit_itemize()
        // produces client-side output via the callback.
        let config = ClientConfig::builder().itemize_changes(true).build();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();

        assert!(
            server_config.flags.info_flags.itemize,
            "itemize_changes should propagate to server config info_flags"
        );
    }

    #[test]
    fn generator_config_itemize_default_false() {
        let config = ClientConfig::default();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();

        assert!(
            !server_config.flags.info_flags.itemize,
            "itemize should be false by default"
        );
    }
}

mod dry_run_remote_close_tests {
    use super::*;
    use std::io;

    #[test]
    fn broken_pipe_is_remote_close() {
        let err = io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe");
        assert!(is_dry_run_remote_close(&err));
    }

    #[test]
    fn connection_reset_is_remote_close() {
        let err = io::Error::new(io::ErrorKind::ConnectionReset, "connection reset");
        assert!(is_dry_run_remote_close(&err));
    }

    #[test]
    fn connection_aborted_is_remote_close() {
        let err = io::Error::new(io::ErrorKind::ConnectionAborted, "connection aborted");
        assert!(is_dry_run_remote_close(&err));
    }

    #[test]
    fn unexpected_eof_is_remote_close() {
        let err = io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected eof");
        assert!(is_dry_run_remote_close(&err));
    }

    #[test]
    fn timeout_is_not_remote_close() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "timed out");
        assert!(!is_dry_run_remote_close(&err));
    }

    #[test]
    fn permission_denied_is_not_remote_close() {
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "permission denied");
        assert!(!is_dry_run_remote_close(&err));
    }

    #[test]
    fn connection_refused_is_not_remote_close() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
        assert!(!is_dry_run_remote_close(&err));
    }

    #[test]
    fn other_error_is_not_remote_close() {
        let err = io::Error::other("some other error");
        assert!(!is_dry_run_remote_close(&err));
    }
}

mod files_from_daemon_args_tests {
    use super::*;
    use crate::client::config::FilesFromSource;
    use std::path::PathBuf;

    fn test_daemon_request() -> DaemonTransferRequest {
        DaemonTransferRequest {
            address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
            module: "test".to_owned(),
            path: String::new(),
            username: None,
        }
    }

    #[test]
    fn push_with_local_file_omits_files_from_arg() {
        // upstream: options.c:2944 - when client is sender and files_from
        // is local, the arg is NOT sent to the daemon.
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--files-from")),
            "push should not send --files-from to daemon: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--from0"),
            "push should not send --from0 to daemon: {args:?}"
        );
    }

    #[test]
    fn push_with_stdin_omits_files_from_arg() {
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::Stdin)
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--files-from")),
            "push with stdin should not send --files-from to daemon: {args:?}"
        );
    }

    #[test]
    fn pull_with_local_file_sends_files_from_stdin() {
        // upstream: options.c:2944 - when client is receiver (pull), local
        // files are forwarded as --files-from=- with --from0.
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            args.iter().any(|a| a == "--files-from=-"),
            "pull should send --files-from=- to daemon: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--from0"),
            "pull should send --from0 to daemon: {args:?}"
        );
    }

    #[test]
    fn pull_with_stdin_sends_files_from_stdin() {
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::Stdin)
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            args.iter().any(|a| a == "--files-from=-"),
            "pull with stdin should send --files-from=- to daemon: {args:?}"
        );
    }

    #[test]
    fn push_with_remote_file_sends_files_from_path() {
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--files-from=/remote/list.txt"),
            "should send remote --files-from path: {args:?}"
        );
    }

    #[test]
    fn pull_with_remote_file_sends_files_from_path() {
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            args.iter().any(|a| a == "--files-from=/remote/list.txt"),
            "should send remote --files-from path: {args:?}"
        );
    }

    #[test]
    fn no_files_from_omits_arg() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--files-from")),
            "should not include --files-from: {args:?}"
        );
    }
}

mod files_from_forwarding_tests {
    use super::*;
    use crate::client::config::{ClientConfigBuilder, FilesFromSource};
    use std::io::Cursor;

    fn test_builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default().transfer_args(["/src", "rsync://host/mod/"])
    }

    #[test]
    fn read_from_local_file_newline_delimited() {
        let dir = test_support::create_tempdir();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "file1.txt\nfile2.txt\nsubdir/file3.txt\n").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader, None).unwrap();
        assert_eq!(
            filenames,
            vec!["file1.txt", "file2.txt", "subdir/file3.txt"]
        );
    }

    #[test]
    fn read_from_local_file_nul_delimited() {
        let dir = test_support::create_tempdir();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "alpha.txt\0beta.txt\0").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .from0(true)
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader, None).unwrap();
        assert_eq!(filenames, vec!["alpha.txt", "beta.txt"]);
    }

    #[test]
    fn read_from_nonexistent_file_returns_error() {
        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(std::path::PathBuf::from(
                "/nonexistent/list.txt",
            )))
            .build();

        let err = read_files_from_for_forwarding(&config).unwrap_err();
        assert!(err.to_string().contains("failed to open --files-from"));
    }

    #[test]
    fn no_forwarding_for_none() {
        let config = test_builder().build();

        let data = read_files_from_for_forwarding(&config).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn no_forwarding_for_remote_file() {
        let config = test_builder()
            .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn empty_local_file_produces_terminator() {
        let dir = test_support::create_tempdir();
        let list_file = dir.path().join("empty.txt");
        std::fs::write(&list_file, "").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        assert_eq!(data, b"\0\0");

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader, None).unwrap();
        assert!(filenames.is_empty());
    }

    #[test]
    fn roundtrip_with_crlf_line_endings() {
        let dir = test_support::create_tempdir();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "file1.txt\r\nfile2.txt\r\n").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader, None).unwrap();
        assert_eq!(filenames, vec!["file1.txt", "file2.txt"]);
    }

    #[test]
    fn files_from_data_on_connection_config() {
        use transfer::config::ConnectionConfig;

        let mut conn = ConnectionConfig::default();
        assert!(conn.files_from_data.is_none());

        conn.files_from_data = Some(b"file1.txt\0file2.txt\0\0".to_vec());
        assert!(conn.files_from_data.is_some());

        let data = conn.files_from_data.take().unwrap();
        assert_eq!(data, b"file1.txt\0file2.txt\0\0");
        assert!(conn.files_from_data.is_none());
    }
}

mod daemon_progress_adapter_tests {
    use super::*;
    use crate::client::progress::{ClientProgressObserver, ClientProgressUpdate};
    use crate::server::{TransferProgressCallback, TransferProgressEvent};
    use std::path::Path;
    use std::time::Instant;

    /// Captures progress updates for assertion in tests.
    struct CapturingObserver {
        updates: Vec<(u64, usize, usize, bool)>,
    }

    impl CapturingObserver {
        fn new() -> Self {
            Self {
                updates: Vec::new(),
            }
        }
    }

    impl ClientProgressObserver for CapturingObserver {
        fn on_progress(&mut self, update: &ClientProgressUpdate) {
            self.updates.push((
                update.overall_transferred(),
                update.index(),
                update.total(),
                update.flist_eof(),
            ));
        }
    }

    #[test]
    fn adapter_accumulates_bytes_across_files() {
        let mut observer = CapturingObserver::new();
        let start = Instant::now();
        let mut adapter = DaemonProgressAdapter::new(&mut observer, start);

        let event1 = TransferProgressEvent {
            path: Path::new("file1.txt"),
            file_bytes: 1000,
            total_file_bytes: Some(1000),
            files_done: 1,
            total_files: 3,
            flist_eof: true,
        };
        adapter.on_file_transferred(&event1);

        let event2 = TransferProgressEvent {
            path: Path::new("file2.txt"),
            file_bytes: 2000,
            total_file_bytes: Some(2000),
            files_done: 2,
            total_files: 3,
            flist_eof: true,
        };
        adapter.on_file_transferred(&event2);

        assert_eq!(observer.updates.len(), 2);
        // First update: 1000 bytes
        assert_eq!(observer.updates[0].0, 1000);
        assert_eq!(observer.updates[0].1, 1);
        assert_eq!(observer.updates[0].2, 3);
        // Second update: 1000 + 2000 = 3000 bytes cumulative
        assert_eq!(observer.updates[1].0, 3000);
        assert_eq!(observer.updates[1].1, 2);
        assert_eq!(observer.updates[1].2, 3);
    }

    #[test]
    fn adapter_forwards_flist_eof_flag() {
        let mut observer = CapturingObserver::new();
        let start = Instant::now();
        let mut adapter = DaemonProgressAdapter::new(&mut observer, start);

        let event = TransferProgressEvent {
            path: Path::new("inc.txt"),
            file_bytes: 500,
            total_file_bytes: Some(500),
            files_done: 1,
            total_files: 2,
            flist_eof: false,
        };
        adapter.on_file_transferred(&event);

        assert_eq!(observer.updates.len(), 1);
        assert!(!observer.updates[0].3, "flist_eof should be false");
    }

    #[test]
    fn adapter_single_file_reports_correct_totals() {
        let mut observer = CapturingObserver::new();
        let start = Instant::now();
        let mut adapter = DaemonProgressAdapter::new(&mut observer, start);

        let event = TransferProgressEvent {
            path: Path::new("only.bin"),
            file_bytes: 4096,
            total_file_bytes: Some(4096),
            files_done: 1,
            total_files: 1,
            flist_eof: true,
        };
        adapter.on_file_transferred(&event);

        assert_eq!(observer.updates.len(), 1);
        assert_eq!(observer.updates[0].0, 4096);
        assert_eq!(observer.updates[0].1, 1);
        assert_eq!(observer.updates[0].2, 1);
        assert!(observer.updates[0].3);
    }

    #[test]
    fn adapter_zero_byte_file_handled() {
        let mut observer = CapturingObserver::new();
        let start = Instant::now();
        let mut adapter = DaemonProgressAdapter::new(&mut observer, start);

        let event = TransferProgressEvent {
            path: Path::new("empty.txt"),
            file_bytes: 0,
            total_file_bytes: Some(0),
            files_done: 1,
            total_files: 2,
            flist_eof: true,
        };
        adapter.on_file_transferred(&event);

        assert_eq!(observer.updates.len(), 1);
        assert_eq!(observer.updates[0].0, 0);
    }
}

mod iconv_bridge {
    //! Integration tests for the `--iconv` bridge from `IconvSetting`
    //! to `ConnectionConfig.iconv` (closes #1911).

    use super::*;
    use crate::client::config::IconvSetting;

    #[test]
    fn unspecified_setting_leaves_connection_iconv_none() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::Unspecified)
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();
        assert!(server_config.connection.iconv.is_none());
    }

    #[test]
    fn disabled_setting_leaves_connection_iconv_none() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::Disabled)
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();
        assert!(server_config.connection.iconv.is_none());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn explicit_setting_populates_connection_iconv_for_receiver() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::Explicit {
                local: "UTF-8".to_owned(),
                remote: Some("ISO-8859-1".to_owned()),
            })
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();
        let converter = server_config
            .connection
            .iconv
            .expect("--iconv=utf-8,latin1 must produce a converter on the receiver path");
        // upstream: rsync.c:130-140 - wire is always UTF-8. When the local
        // charset matches the wire charset, this peer's converter is
        // identity (no transcoding on the local->wire direction). The
        // `remote` half of LOCAL,REMOTE is the peer's local charset and is
        // forwarded to the remote CLI separately, not consumed here.
        assert!(converter.is_identity());
        assert_eq!(converter.local_encoding_name(), "UTF-8");
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn explicit_setting_populates_connection_iconv_for_generator() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::Explicit {
                local: "UTF-8".to_owned(),
                remote: Some("ISO-8859-1".to_owned()),
            })
            .build();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();
        assert!(server_config.connection.iconv.is_some());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn locale_default_setting_populates_connection_iconv() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::LocaleDefault)
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();
        let converter = server_config
            .connection
            .iconv
            .expect("--iconv=. must produce a locale-derived converter");
        // converter_from_locale uses UTF-8 on both sides for portability,
        // making it an identity converter on most modern systems.
        assert!(converter.is_identity());
    }
}
