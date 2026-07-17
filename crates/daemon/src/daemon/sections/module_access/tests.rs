#[cfg(test)]
mod module_access_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn generate_auth_challenge_includes_ip_and_timestamp() {
        let peer_ip = "192.168.1.1".parse::<IpAddr>().unwrap();
        let challenge = generate_auth_challenge(peer_ip, Some(ProtocolVersion::V32));

        // Challenge should be base64-encoded hash (22 characters without padding)
        assert_eq!(challenge.len(), 22);
        assert!(
            challenge
                .chars()
                .all(|c| c.is_alphanumeric() || c == '+' || c == '/')
        );
    }

    #[test]
    fn generate_auth_challenge_uses_md4_for_legacy_protocol() {
        let peer_ip = "192.168.1.1".parse::<IpAddr>().unwrap();
        let challenge = generate_auth_challenge(peer_ip, Some(ProtocolVersion::V29));

        // MD4 also produces 16-byte hash = 22 base64 characters
        assert_eq!(challenge.len(), 22);
        assert!(
            challenge
                .chars()
                .all(|c| c.is_alphanumeric() || c == '+' || c == '/')
        );
    }

    #[test]
    fn generate_auth_challenge_produces_different_values() {
        let peer_ip = "10.0.0.1".parse::<IpAddr>().unwrap();
        let challenge1 = generate_auth_challenge(peer_ip, Some(ProtocolVersion::V32));

        // Retry until the microsecond timestamp changes (bounded)
        let mut challenge2 = challenge1.clone();
        for i in 0..200 {
            challenge2 = generate_auth_challenge(peer_ip, Some(ProtocolVersion::V32));
            if challenge2 != challenge1 {
                break;
            }
            assert!(
                i < 199,
                "challenge did not change after 200 retries"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        assert_ne!(challenge1, challenge2);
    }

    #[test]
    fn sanitize_module_identifier_preserves_clean_input() {
        let clean = "my_module-123";
        let result = sanitize_module_identifier(clean);
        assert_eq!(result, clean);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn sanitize_module_identifier_replaces_control_characters() {
        let dirty = "module\nwith\tcontrols\r";
        let result = sanitize_module_identifier(dirty);
        assert_eq!(result, "module?with?controls?");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn sanitize_module_identifier_handles_mixed_content() {
        let mixed = "mod\x00ule_\x1bname";
        let result = sanitize_module_identifier(mixed);
        assert_eq!(result, "mod?ule_?name");
    }

    #[test]
    fn read_client_arguments_protocol_30_null_terminated() {
        let input = b"--server\0--sender\0-r\0.\0\0";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server", "--sender", "-r", "."]);
    }

    #[test]
    fn read_client_arguments_protocol_30_stops_at_empty() {
        let input = b"--server\0\0more\0data\0";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server"]);
    }

    #[test]
    fn read_client_arguments_protocol_29_newline_terminated() {
        let input = b"--server\n--sender\n-r\n.\n\n";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V29))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server", "--sender", "-r", "."]);
    }

    #[test]
    fn read_client_arguments_protocol_29_stops_at_empty_line() {
        let input = b"--server\n\nmore\n";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V29))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server"]);
    }

    #[test]
    fn read_client_arguments_handles_eof() {
        let input = b"--server\0--sender\0";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server", "--sender"]);
    }

    #[test]
    fn read_client_arguments_empty_input() {
        let input = b"";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("should read arguments");

        assert!(args.is_empty());
    }

    // upstream: io.c:1295-1306 unbackslash_arg().
    #[test]
    fn unbackslash_arg_collapses_backslash_escapes() {
        assert_eq!(unbackslash_arg("plain"), "plain");
        assert_eq!(unbackslash_arg("\\*"), "*");
        assert_eq!(unbackslash_arg("\\;\\&\\|"), ";&|");
        assert_eq!(
            unbackslash_arg("--groupmap=\\*:1234\\;dangerous"),
            "--groupmap=*:1234;dangerous"
        );
        // A lone trailing backslash is preserved verbatim, matching upstream's
        // `if (*f == '\\' && f[1])` guard.
        assert_eq!(unbackslash_arg("trailing\\"), "trailing\\");
        // Double backslash escapes to single, mirroring upstream.
        assert_eq!(unbackslash_arg("\\\\"), "\\");
    }

    // upstream: io.c:1336-1359 - unescape applies only to args before the `.`
    // CWD marker; file args after the dot pass through verbatim because the
    // upstream loop dispatches them through glob_expand() instead.
    #[test]
    fn unescape_phase1_option_args_stops_at_dot_marker() {
        let args = vec![
            "--server".to_owned(),
            "--groupmap=\\*:1234".to_owned(),
            ".".to_owned(),
            "module/file\\*".to_owned(),
        ];
        let out = unescape_phase1_option_args(args);
        assert_eq!(
            out,
            vec![
                "--server".to_owned(),
                "--groupmap=*:1234".to_owned(),
                ".".to_owned(),
                "module/file\\*".to_owned(),
            ]
        );
    }

    // upstream: clientserver.c:1073 - first read_args() call passes
    // `unescape=1` so a non-protect daemon receiver round-trips shell-escaped
    // option values. Without this, --groupmap=*:1234 sent under non-protect
    // arrives at the daemon as the literal "\*:1234".
    #[test]
    fn unescape_phase1_option_args_no_dot_marker_unescapes_all() {
        let args = vec![
            "--usermap=\\*:5678".to_owned(),
            "--groupmap=\\*:1234\\;dangerous".to_owned(),
        ];
        let out = unescape_phase1_option_args(args);
        assert_eq!(
            out,
            vec![
                "--usermap=*:5678".to_owned(),
                "--groupmap=*:1234;dangerous".to_owned(),
            ]
        );
    }

    // upstream: clientserver.c:1073,1083 - the daemon parses BOTH phase 1
    // (cmdline) and phase 2 (stdin / secluded-args) and the union of their
    // options drives the transfer. The compact flag string (`-slogDtprIzxe...`)
    // and the role marker `--sender` live in phase 1; long-form options such
    // as `--groupmap=*:GID` live in phase 2. Dropping phase 1 (the prior
    // oc-rsync behaviour) silently removed `-l`, `-r`, `-z`, `--sender`, ...
    // and broke `daemon-groupmap-wild` under secluded-args mode because
    // compression negotiation diverged before the transfer could start.
    #[test]
    fn merge_secluded_args_prepends_phase1_and_skips_rsync_arg0() {
        let phase1 = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "-slogDtprIze.LsfxCIvu".to_owned(),
            "--iconv=UTF-8".to_owned(),
        ];
        let phase2 = vec![
            "rsync".to_owned(),
            "--log-format=%i".to_owned(),
            "--groupmap=*:1000".to_owned(),
            ".".to_owned(),
            "upload/".to_owned(),
        ];
        let merged = merge_secluded_args(phase1, phase2);
        assert_eq!(
            merged,
            vec![
                "--server".to_owned(),
                "--sender".to_owned(),
                "-slogDtprIze.LsfxCIvu".to_owned(),
                "--iconv=UTF-8".to_owned(),
                "--log-format=%i".to_owned(),
                "--groupmap=*:1000".to_owned(),
                ".".to_owned(),
                "upload/".to_owned(),
            ],
        );
    }

    // Phase 2 wire output drops the "rsync" arg0 only when it really is at
    // index 0 (upstream `rsync.c:295` `args[i] = "rsync"`). A legitimate
    // user-visible arg literally equal to "rsync" never appears at index 0
    // of phase 2 because upstream emits it only as the synthetic arg0; the
    // first arg the client supplies is `--server`, so the heuristic is safe.
    #[test]
    fn merge_secluded_args_passes_phase2_through_when_no_synthetic_arg0() {
        let phase1 = vec!["--server".to_owned(), "-logDtpr".to_owned()];
        let phase2 = vec![".".to_owned(), "mod/".to_owned()];
        let merged = merge_secluded_args(phase1, phase2);
        assert_eq!(
            merged,
            vec![
                "--server".to_owned(),
                "-logDtpr".to_owned(),
                ".".to_owned(),
                "mod/".to_owned(),
            ],
        );
    }

    // upstream issue #829: under secluded-args mode the client emits
    // `--groupmap=*:GID` literally on the phase 2 wire (`safe_arg()` skips
    // the WILD_CHARS escape when `protect_args` is set, `options.c:2551`).
    // After `merge_secluded_args` the daemon's `apply_long_form_args` sees
    // the wildcard intact and `GroupMapping::parse` consumes it without
    // rejecting the `*` matcher.
    //
    // Windows lacks POSIX user/group concepts so the metadata crate ships a
    // `GroupMapping::parse` stub that always returns `Err` (see
    // `metadata/src/mapping_win.rs`). The assertion that `config.group_mapping`
    // is populated therefore only applies on Unix targets.
    #[cfg(unix)]
    #[test]
    fn merge_secluded_args_preserves_groupmap_wildcard_through_apply_long_form() {
        let phase1 = vec!["--server".to_owned(), "-logDtpr".to_owned()];
        let phase2 = vec![
            "rsync".to_owned(),
            "--groupmap=*:1234".to_owned(),
            ".".to_owned(),
            "upload/".to_owned(),
        ];
        let merged = merge_secluded_args(phase1, phase2);

        let mut config = ServerConfig::default();
        let unknown = apply_long_form_args(&merged, &mut config);
        assert!(unknown.is_none());
        let mapping = config.group_mapping.expect("groupmap should be parsed");
        assert_eq!(mapping.spec(), "*:1234");
    }

    // upstream: options.c:2345-2348 - the daemon parses the client-forwarded
    // --log-format to set stdout_format_has_i. `%i` enables itemize of
    // significant items; `%i%I` is the `-ii` level that also itemizes unchanged
    // entries. Without the `%I` -> itemize_unchanged mapping a `-ii` push to an
    // oc daemon drops every unchanged row.
    #[test]
    fn apply_long_form_args_maps_log_format_itemize_levels() {
        let single = vec!["--log-format=%i".to_owned()];
        let mut cfg = ServerConfig::default();
        assert!(apply_long_form_args(&single, &mut cfg).is_none());
        assert!(cfg.flags.info_flags.itemize, "%i enables itemize");
        assert!(
            !cfg.flags.info_flags.itemize_unchanged,
            "%i alone must not itemize unchanged entries",
        );

        let double = vec!["--log-format=%i%I".to_owned()];
        let mut cfg2 = ServerConfig::default();
        assert!(apply_long_form_args(&double, &mut cfg2).is_none());
        assert!(cfg2.flags.info_flags.itemize, "%i%I enables itemize");
        assert!(
            cfg2.flags.info_flags.itemize_unchanged,
            "%I raises the itemize level to -ii (unchanged rows)",
        );
    }

    // UTS-8.REOPEN regression: the client's actual phase-1 wire for
    // secluded-args daemon push is `[--server, --sender, --secluded-args]`
    // (no standalone `.` or bare `-s`), and phase 2 carries the real
    // compact flag string plus `--groupmap=*:GID`. The previous client
    // emitted a stray `.` in phase 1 which made `apply_long_form_args`'
    // first-`.` dot_position lookup short-circuit the option region, so
    // `--groupmap` was silently dropped before reaching
    // `GroupMapping::parse`. The previous client also emitted a bare `-s`
    // in phase 1 which shadowed phase 2's real compact flag string in
    // `build_server_config`'s first-short-form-arg picker, breaking
    // compression / recursion negotiation. The merged arg list emitted
    // by the fixed client must round-trip `--groupmap=*:GID` intact AND
    // expose phase 2's real compact flag string as the first short-form
    // arg so the daemon's option region parser sees both correctly.
    //
    // upstream: clientserver.c:395-402 phase 1 wire layout
    // upstream: clientserver.c:303 `.` and module path land in phase 2
    // upstream: options.c:2744-2745 NULL marker between phase 1 and phase 2
    // upstream: options.c:804 `--secluded-args` long-form alias of `-s`
    //
    // Windows lacks POSIX user/group concepts so the metadata crate ships a
    // `GroupMapping::parse` stub that always returns `Err` (see
    // `metadata/src/mapping_win.rs`). The assertion that `config.group_mapping`
    // is populated therefore only applies on Unix targets.
    #[cfg(unix)]
    #[test]
    fn merge_secluded_args_oc_rsync_client_wire_preserves_groupmap_wildcard() {
        // Phase 1 mirrors the fixed `build_minimal_daemon_args` output
        // for a daemon-push (client is sender, daemon is receiver).
        let phase1 = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "--secluded-args".to_owned(),
        ];
        // Phase 2 mirrors `build_full_daemon_args` output: leading
        // synthetic "rsync" arg0, then `--server`, `--sender`, the real
        // compact flag string, the long-form options including the
        // wildcard `--groupmap`, the `.` separator, and the positional
        // module path. The leading `--server` / `--sender` are duplicated
        // because oc-rsync builds the full arg list once and ships it in
        // phase 2 (vs upstream which splits server_options() output at
        // the NULL marker). The duplication is harmless: `apply_long_form_args`
        // ignores `--server` / `--sender` (role determination uses a
        // separate scan).
        let phase2 = vec![
            "rsync".to_owned(),
            "--server".to_owned(),
            "--sender".to_owned(),
            "-logDtprIze.LsfxCIvu".to_owned(),
            "--log-format=%i".to_owned(),
            "--groupmap=*:4242".to_owned(),
            ".".to_owned(),
            "upload/".to_owned(),
        ];
        let merged = merge_secluded_args(phase1, phase2);

        // `apply_long_form_args` finds the first standalone `.` at the
        // expected position (after `--groupmap`), so the wildcard option
        // is parsed instead of being treated as a positional file arg.
        let mut config = ServerConfig::default();
        let unknown = apply_long_form_args(&merged, &mut config);
        assert!(
            unknown.is_none(),
            "no client-only batch flag should reach the daemon",
        );
        let mapping = config
            .group_mapping
            .expect("groupmap=*:4242 must reach GroupMapping::parse intact");
        assert_eq!(mapping.spec(), "*:4242");

        // The real compact flag string is the first short-form arg in the
        // merged list (the daemon's `build_server_config` picks the first
        // arg matching `starts_with('-') && !starts_with("--")`). A bare
        // `-s` in phase 1 would have shadowed it; `--secluded-args` is
        // long-form and so does not.
        let first_short = merged
            .iter()
            .find(|a| a.starts_with('-') && !a.starts_with("--"))
            .expect("merged args must include a short-form compact flag string");
        assert_eq!(first_short, "-logDtprIze.LsfxCIvu");
    }

    #[test]
    fn apply_module_bandwidth_limit_disables_when_module_configured_none() {
        let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            None,
            false,
            true, // module_limit_configured
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Disabled);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_module_bandwidth_limit_preserves_when_not_configured() {
        let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            None,
            false,
            false, // module_limit_configured
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Unchanged);
        assert!(limiter.is_some());
    }

    #[test]
    fn apply_module_bandwidth_limit_enables_when_none_existed() {
        let mut limiter = None;

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            NonZeroU64::new(2048),
            true, // module_limit_specified
            true, // module_limit_configured
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Enabled);
        assert!(limiter.is_some());
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 2048);
    }

    #[test]
    fn apply_module_bandwidth_limit_lowers_existing_limit() {
        let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(2048).unwrap()));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            NonZeroU64::new(1024),
            true,
            true,
            None,
            false,
        );

        // Lowering the limit results in Updated
        assert_eq!(change, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1024);
    }

    #[test]
    fn apply_module_bandwidth_limit_unchanged_when_limit_higher() {
        let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            NonZeroU64::new(2048),
            true,
            true,
            None,
            false,
        );

        // Higher limit doesn't raise existing limit (cap function), so Unchanged
        assert_eq!(change, LimiterChange::Unchanged);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1024);
    }

    #[test]
    fn apply_module_bandwidth_limit_burst_only_override() {
        let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            None,
            false,
            true, // module_limit_configured
            NonZeroU64::new(4096),
            true, // module_burst_specified
        );

        // Should update with burst
        assert_eq!(change, LimiterChange::Updated);
        assert!(limiter.is_some());
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 4096);
    }

    #[test]
    fn format_bandwidth_rate_displays_bytes() {
        let rate = NonZeroU64::new(512).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "512 bytes/s");
    }

    #[test]
    fn format_bandwidth_rate_displays_kib() {
        let rate = NonZeroU64::new(2048).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "2 KiB/s");
    }

    #[test]
    fn format_bandwidth_rate_displays_mib() {
        let rate = NonZeroU64::new(5 * 1024 * 1024).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "5 MiB/s");
    }

    #[test]
    fn format_bandwidth_rate_displays_gib() {
        let rate = NonZeroU64::new(3 * 1024 * 1024 * 1024).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "3 GiB/s");
    }

    #[test]
    fn format_bandwidth_rate_prefers_largest_unit() {
        let rate = NonZeroU64::new(1024).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "1 KiB/s");

        let rate = NonZeroU64::new(1025).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "1025 bytes/s");
    }

    /// upstream: log.c:163 - log-open failures produce RERR_MESSAGEIO (13).
    #[test]
    fn log_file_error_creates_daemon_error_with_correct_code() {
        let path = std::path::Path::new("/tmp/test.log");
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "test error");
        let err = log_file_error(path, io_err);
        assert_eq!(err.exit_code(), core::exit_code::ExitCode::MessageIo.as_i32());
    }

    #[test]
    fn log_file_error_message_contains_path() {
        let path = std::path::Path::new("/var/log/rsyncd.log");
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err = log_file_error(path, io_err);
        let message = format!("{:?}", err.message());
        assert!(message.contains("/var/log/rsyncd.log"));
    }

    #[test]
    fn pid_file_error_creates_daemon_error_with_correct_code() {
        let path = std::path::Path::new("/var/run/rsyncd.pid");
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "test error");
        let err = pid_file_error(path, io_err);
        assert_eq!(err.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    }

    #[test]
    fn pid_file_error_message_contains_path() {
        let path = std::path::Path::new("/var/run/rsyncd.pid");
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = pid_file_error(path, io_err);
        let message = format!("{:?}", err.message());
        assert!(message.contains("/var/run/rsyncd.pid"));
    }

    #[test]
    fn lock_file_error_creates_daemon_error_with_correct_code() {
        let path = std::path::Path::new("/var/lock/rsyncd.lock");
        let io_err = std::io::Error::new(std::io::ErrorKind::AlreadyExists, "locked");
        let err = lock_file_error(path, io_err);
        assert_eq!(err.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    }

    #[test]
    fn lock_file_error_message_contains_path() {
        let path = std::path::Path::new("/var/lock/rsyncd.lock");
        let io_err = std::io::Error::new(std::io::ErrorKind::AlreadyExists, "file locked");
        let err = lock_file_error(path, io_err);
        let message = format!("{:?}", err.message());
        assert!(message.contains("/var/lock/rsyncd.lock"));
    }

    #[test]
    fn format_host_returns_hostname_when_present() {
        use std::net::IpAddr;
        let host = Some("example.com");
        let fallback: IpAddr = "192.168.1.1".parse().unwrap();
        assert_eq!(format_host(host, fallback), "example.com");
    }

    #[test]
    fn format_host_returns_ip_when_hostname_missing() {
        use std::net::IpAddr;
        let host: Option<&str> = None;
        let fallback: IpAddr = "10.0.0.1".parse().unwrap();
        assert_eq!(format_host(host, fallback), "10.0.0.1");
    }

    #[test]
    fn format_host_returns_ipv6_when_hostname_missing() {
        use std::net::IpAddr;
        let host: Option<&str> = None;
        let fallback: IpAddr = "::1".parse().unwrap();
        assert_eq!(format_host(host, fallback), "::1");
    }

    #[test]
    fn determine_server_role_sender_when_sender_flag_present() {
        let args = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "-r".to_owned(),
        ];
        assert!(matches!(
            determine_server_role(&args),
            ServerRole::Generator
        ));
    }

    #[test]
    fn determine_server_role_receiver_when_sender_flag_absent() {
        let args = vec!["--server".to_owned(), "-r".to_owned()];
        assert!(matches!(determine_server_role(&args), ServerRole::Receiver));
    }

    #[test]
    fn determine_server_role_receiver_when_empty() {
        let args: Vec<String> = vec![];
        assert!(matches!(determine_server_role(&args), ServerRole::Receiver));
    }

    // upstream: clientserver.c:1254 governs the format_module_listing_line
    // wire layout exercised below.

    #[test]
    fn module_listing_format_short_name_padded_to_15() {
        // upstream: %-15s pads short names with trailing spaces
        let line = format_module_listing_line("docs", "Documentation");
        assert_eq!(line, "docs           \tDocumentation\n");
    }

    #[test]
    fn module_listing_format_exact_15_char_name() {
        // A name exactly 15 characters wide should have no extra padding
        let line = format_module_listing_line("exactly15chars_", "comment");
        assert_eq!(line, "exactly15chars_\tcomment\n");
    }

    #[test]
    fn module_listing_format_name_longer_than_15() {
        // upstream: %-15s does not truncate - names wider than 15 chars extend the field
        let line = format_module_listing_line("very_long_module_name", "A long name module");
        assert_eq!(line, "very_long_module_name\tA long name module\n");
    }

    #[test]
    fn module_listing_format_empty_comment() {
        // upstream: lp_comment(i) returns "" for modules without a comment directive
        let line = format_module_listing_line("backup", "");
        assert_eq!(line, "backup         \t\n");
    }

    #[test]
    fn module_listing_format_single_char_name() {
        let line = format_module_listing_line("x", "tiny");
        assert_eq!(line, "x              \ttiny\n");
    }

    #[test]
    fn module_listing_format_empty_name() {
        // Edge case: empty module name still gets padded to 15 spaces
        let line = format_module_listing_line("", "orphan");
        assert_eq!(line, "               \torphan\n");
    }

    #[test]
    fn module_listing_format_tab_separator_present() {
        // The separator between name field and comment must be exactly one tab
        let line = format_module_listing_line("test", "hello");
        let parts: Vec<&str> = line.trim_end_matches('\n').splitn(2, '\t').collect();
        assert_eq!(
            parts.len(),
            2,
            "line must contain exactly one tab separator"
        );
        assert_eq!(parts[0], "test           ");
        assert_eq!(parts[1], "hello");
    }

    #[test]
    fn module_listing_format_terminates_with_newline() {
        let line = format_module_listing_line("mod", "comment");
        assert!(line.ends_with('\n'), "line must end with newline");
        assert!(!line.ends_with("\n\n"), "line must not have double newline");
    }


    #[cfg(unix)]
    #[test]
    fn check_permissions_accepts_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "alice:pass\n").expect("write");
        fs::set_permissions(&secrets, PermissionsExt::from_mode(0o600)).expect("chmod");

        assert!(check_secrets_file_permissions(&secrets).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn check_permissions_rejects_other_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "alice:pass\n").expect("write");
        fs::set_permissions(&secrets, PermissionsExt::from_mode(0o604)).expect("chmod");

        let err = check_secrets_file_permissions(&secrets).expect_err("should reject");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("must not be other-accessible"));
    }

    #[cfg(unix)]
    #[test]
    fn check_permissions_rejects_other_writable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "alice:pass\n").expect("write");
        fs::set_permissions(&secrets, PermissionsExt::from_mode(0o602)).expect("chmod");

        let err = check_secrets_file_permissions(&secrets).expect_err("should reject");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn check_permissions_allows_group_readable_without_other() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "alice:pass\n").expect("write");
        // upstream: authenticate.c only checks `(mode & 06)` - group bits are allowed
        fs::set_permissions(&secrets, PermissionsExt::from_mode(0o640)).expect("chmod");

        assert!(check_secrets_file_permissions(&secrets).is_ok());
    }


    #[cfg(unix)]
    #[test]
    fn verify_secret_rejects_other_accessible_when_strict_modes_enabled() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "alice:password123\n").expect("write");
        fs::set_permissions(&secrets, PermissionsExt::from_mode(0o644)).expect("chmod");

        let module = ModuleDefinition {
            secrets_file: Some(secrets),
            strict_modes: true,
            ..Default::default()
        };

        // upstream: authenticate.c:119-131 check_secret() - a strict-modes
        // violation is an auth denial, not a fatal error: verify returns
        // Ok(false) so the daemon emits `@ERROR: auth failed on module X`
        // rather than dropping the socket mid-handshake.
        let result = verify_secret_response(&module, "alice", None, "challenge", "response", None)
            .expect("strict-modes violation must be a denial, not an io error");
        assert!(
            !result,
            "other-accessible secrets under strict modes must deny auth"
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_secret_accepts_other_accessible_when_strict_modes_disabled() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "alice:password123\n").expect("write");
        fs::set_permissions(&secrets, PermissionsExt::from_mode(0o644)).expect("chmod");

        let module = ModuleDefinition {
            secrets_file: Some(secrets),
            strict_modes: false,
            ..Default::default()
        };

        // With strict_modes disabled, the file is read even though it's world-readable.
        // Authentication will fail (wrong response), but no permission error is returned.
        let result = verify_secret_response(&module, "alice", None, "challenge", "response", None)
            .expect("should not error on permissions");
        assert!(
            !result,
            "auth should fail due to wrong response, not permissions"
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_secret_succeeds_with_strict_modes_and_correct_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "alice:password123\n").expect("write");
        fs::set_permissions(&secrets, PermissionsExt::from_mode(0o600)).expect("chmod");

        let module = ModuleDefinition {
            secrets_file: Some(secrets),
            strict_modes: true,
            ..Default::default()
        };

        // Permissions are fine, so the file is read. Auth will fail (wrong response)
        // but no permission error is returned.
        let result = verify_secret_response(&module, "alice", None, "challenge", "response", None)
            .expect("should not error on permissions");
        assert!(!result, "auth should fail due to wrong response");
    }

    /// Computes the client digest a member of the authorizing group (or the
    /// user) would send for `secret`, so the shared-secret tests below assert
    /// real authentication rather than a hard-coded string.
    fn client_digest(secret: &str, challenge: &str) -> String {
        core::auth::compute_daemon_auth_response(
            secret.as_bytes(),
            challenge,
            core::auth::DaemonAuthDigest::Md5,
        )
    }

    /// A shared `@group:secret` line authenticates a member authorized through
    /// that same group token. Upstream matches such a line against the group
    /// name that `auth users` resolved, so the shared entry is the credential
    /// for every member - without this, a `auth users = @grp` + `@grp:pass`
    /// config would authorize the user then wrongly deny at the secret lookup.
    ///
    /// upstream: authenticate.c:145-156 - an `@`-prefixed secrets key is matched
    /// against the authorizing group rather than the username.
    #[test]
    fn verify_secret_matches_group_line_for_group_member() {
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "@devs:groupsecret\n").expect("write");

        let module = ModuleDefinition {
            secrets_file: Some(secrets),
            strict_modes: false,
            ..Default::default()
        };

        let challenge = "challenge";
        let response = client_digest("groupsecret", challenge);

        // Authorized via the `@devs` token: the group line is the credential.
        let granted =
            verify_secret_response(&module, "alice", Some("devs"), challenge, &response, None)
                .expect("no io error");
        assert!(granted, "group member must authenticate via @devs shared secret");

        // upstream: authenticate.c:318 - a plain-username authorization passes a
        // NULL group, so `@group:` lines are never consulted. Denied here.
        let denied = verify_secret_response(&module, "alice", None, challenge, &response, None)
            .expect("no io error");
        assert!(
            !denied,
            "a @group secret must not match when the user was not authorized via that group"
        );
    }

    /// Duplicate username entries: the first key-matching line decides the
    /// outcome. An earlier wrong-password line retires the username, so a later
    /// correct-password line for the same user cannot flip the denial. This
    /// mirrors upstream setting the name pointer to NULL on mismatch.
    ///
    /// upstream: authenticate.c:158-162 - on password mismatch `err =
    /// "password mismatch"; *ptr = NULL;` ends the search for that name.
    #[test]
    fn verify_secret_first_username_match_wins() {
        let dir = tempfile::tempdir().expect("temp dir");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "alice:wrongpass\nalice:rightpass\n").expect("write");

        let module = ModuleDefinition {
            secrets_file: Some(secrets),
            strict_modes: false,
            ..Default::default()
        };

        let challenge = "challenge";
        let response = client_digest("rightpass", challenge);

        // The first `alice:` line mismatches, retiring the username; the later
        // `alice:rightpass` duplicate must NOT authenticate.
        let denied = verify_secret_response(&module, "alice", None, challenge, &response, None)
            .expect("no io error");
        assert!(
            !denied,
            "an earlier wrong-password line must retire the username and deny"
        );

        // Control: when the first line is the correct one, auth succeeds.
        let secrets_ok = dir.path().join("secrets_ok");
        fs::write(&secrets_ok, "alice:rightpass\nalice:wrongpass\n").expect("write");
        let module_ok = ModuleDefinition {
            secrets_file: Some(secrets_ok),
            strict_modes: false,
            ..Default::default()
        };
        let granted =
            verify_secret_response(&module_ok, "alice", None, challenge, &response, None)
                .expect("no io error");
        assert!(granted, "a correct first line must authenticate");
    }

    #[test]
    fn read_client_arguments_normal_protocol30() {
        let data = b"--server\0--sender\0-logDtpr\0.\0mod/path\0\0";
        let mut cursor = Cursor::new(&data[..]);
        let mut reader = std::io::BufReader::new(&mut cursor);
        let args =
            read_client_arguments(&mut reader, Some(ProtocolVersion::V32)).expect("should parse");
        assert_eq!(
            args,
            vec!["--server", "--sender", "-logDtpr", ".", "mod/path"]
        );
    }

    #[test]
    fn read_client_arguments_with_secluded_flag() {
        // Phase 1: minimal args with -s
        // Phase 2: full args via secluded-args wire format
        let mut data = Vec::new();
        // Phase 1: --server\0-s\0.\0\0
        data.extend_from_slice(b"--server\0-s\0.\0\0");
        // Phase 2: rsync\0--server\0--sender\0-logDtpr\0.\0mod/path\0\0
        data.extend_from_slice(b"rsync\0--server\0--sender\0-logDtpr\0.\0mod/path\0\0");

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = std::io::BufReader::new(&mut cursor);

        // read_client_arguments only reads phase 1
        let phase1 =
            read_client_arguments(&mut reader, Some(ProtocolVersion::V32)).expect("should parse");
        assert_eq!(phase1, vec!["--server", "-s", "."]);

        // Detect secluded flag
        assert!(has_secluded_args_flag(&phase1));

        // Read phase 2
        let full_args = protocol::secluded_args::recv_secluded_args(&mut reader, None)
            .expect("should read secluded args");
        assert_eq!(full_args[0], "rsync");
        let effective: Vec<&str> = full_args.iter().skip(1).map(String::as_str).collect();
        assert_eq!(
            effective,
            vec!["--server", "--sender", "-logDtpr", ".", "mod/path"]
        );
    }

    #[test]
    fn read_client_arguments_legacy_protocol29() {
        let data = b"--server\n--sender\n-logDtpr\n.\nmod/path\n\n";
        let mut cursor = Cursor::new(&data[..]);
        let mut reader = std::io::BufReader::new(&mut cursor);
        let args =
            read_client_arguments(&mut reader, Some(ProtocolVersion::V29)).expect("should parse");
        assert_eq!(
            args,
            vec!["--server", "--sender", "-logDtpr", ".", "mod/path"]
        );
    }


    #[test]
    fn apply_long_form_args_parses_temp_dir_separate_args() {
        let args = vec![
            "--server".to_owned(),
            "--temp-dir".to_owned(),
            "/tmp/rsync-temp".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(
            config.temp_dir.as_deref(),
            Some(std::path::Path::new("/tmp/rsync-temp"))
        );
    }

    #[test]
    fn apply_long_form_args_parses_temp_dir_equals_format() {
        let args = vec![
            "--server".to_owned(),
            "--temp-dir=/staging/area".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(
            config.temp_dir.as_deref(),
            Some(std::path::Path::new("/staging/area"))
        );
    }

    #[test]
    fn apply_long_form_args_temp_dir_defaults_to_none() {
        let args = vec!["--server".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.temp_dir.is_none());
    }


    #[test]
    fn apply_long_form_args_parses_compare_dest_equals_format() {
        let args = vec![
            "--server".to_owned(),
            "--compare-dest=/snapshots/daily".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.reference_directories.len(), 1);
        assert_eq!(
            config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Compare
        );
        assert_eq!(
            config.reference_directories[0].path(),
            std::path::Path::new("/snapshots/daily")
        );
    }

    #[test]
    fn apply_long_form_args_parses_compare_dest_separate_args() {
        let args = vec![
            "--server".to_owned(),
            "--compare-dest".to_owned(),
            "/snapshots/daily".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.reference_directories.len(), 1);
        assert_eq!(
            config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Compare
        );
        assert_eq!(
            config.reference_directories[0].path(),
            std::path::Path::new("/snapshots/daily")
        );
    }

    #[test]
    fn apply_long_form_args_parses_link_dest_equals_format() {
        let args = vec![
            "--server".to_owned(),
            "--link-dest=/prev/backup".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.reference_directories.len(), 1);
        assert_eq!(
            config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Link
        );
        assert_eq!(
            config.reference_directories[0].path(),
            std::path::Path::new("/prev/backup")
        );
    }

    #[test]
    fn apply_long_form_args_parses_link_dest_separate_args() {
        let args = vec![
            "--server".to_owned(),
            "--link-dest".to_owned(),
            "/prev/backup".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.reference_directories.len(), 1);
        assert_eq!(
            config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Link
        );
        assert_eq!(
            config.reference_directories[0].path(),
            std::path::Path::new("/prev/backup")
        );
    }

    #[test]
    fn apply_long_form_args_parses_copy_dest_equals_format() {
        let args = vec![
            "--server".to_owned(),
            "--copy-dest=/cache/warm".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.reference_directories.len(), 1);
        assert_eq!(
            config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Copy
        );
        assert_eq!(
            config.reference_directories[0].path(),
            std::path::Path::new("/cache/warm")
        );
    }

    #[test]
    fn apply_long_form_args_parses_multiple_link_dests() {
        let args = vec![
            "--server".to_owned(),
            "--link-dest=/prev1".to_owned(),
            "--link-dest=/prev2".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.reference_directories.len(), 2);
        assert_eq!(
            config.reference_directories[0].path(),
            std::path::Path::new("/prev1")
        );
        assert_eq!(
            config.reference_directories[1].path(),
            std::path::Path::new("/prev2")
        );
    }

    #[test]
    fn apply_long_form_args_reference_dirs_default_empty() {
        let args = vec!["--server".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.reference_directories.is_empty());
    }

    // upstream: options.c:2750-2761 - server_options() sends --log-format=%i
    // when the client uses -i/--itemize-changes. The daemon must parse this
    // to set info_flags.itemize so the receiver emits MSG_INFO itemize frames.

    #[test]
    fn apply_long_form_args_parses_log_format_with_itemize() {
        let args = vec!["--server".to_owned(), "--log-format=%i".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_parses_log_format_with_itemize_and_upper_i() {
        let args = vec!["--server".to_owned(), "--log-format=%i%I".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_parses_out_format_with_itemize() {
        let args = vec!["--server".to_owned(), "--out-format=%i".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_log_format_without_itemize() {
        let args = vec!["--server".to_owned(), "--log-format=%o".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(!config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_log_format_x_no_itemize() {
        let args = vec!["--server".to_owned(), "--log-format=X".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(!config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_parses_delay_updates() {
        let args = vec![
            "--server".to_owned(),
            "--delay-updates".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.write.delay_updates);
    }

    #[test]
    fn apply_long_form_args_delay_updates_defaults_to_false() {
        let args = vec!["--server".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(!config.write.delay_updates);
    }

    // NSV: the daemon-sender opts its socket write side into io_uring SEND_ZC
    // only when the client forwarded `--zero-copy`. The flag maps to
    // `write.zero_copy_policy = Enabled`, which `setup_transfer_streams`
    // consults to choose the zero-copy writer.
    #[test]
    fn apply_long_form_args_parses_zero_copy_enabled() {
        let args = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "--zero-copy".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let unknown = apply_long_form_args(&args, &mut config);
        assert!(unknown.is_none(), "--zero-copy must be a known daemon flag");
        assert_eq!(
            config.write.zero_copy_policy,
            fast_io::ZeroCopyPolicy::Enabled
        );
    }

    #[test]
    fn apply_long_form_args_parses_no_zero_copy_disabled() {
        let args = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "--no-zero-copy".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let unknown = apply_long_form_args(&args, &mut config);
        assert!(unknown.is_none());
        assert_eq!(
            config.write.zero_copy_policy,
            fast_io::ZeroCopyPolicy::Disabled
        );
    }

    // HARD default-path invariant at the daemon parse boundary: absent the
    // flag, the policy stays `Auto`, so the daemon keeps its current writer.
    #[test]
    fn apply_long_form_args_zero_copy_defaults_to_auto() {
        let args = vec!["--server".to_owned(), ".".to_owned()];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.write.zero_copy_policy, fast_io::ZeroCopyPolicy::Auto);
    }

    // Byte-identical wire-transcript gate: the SEND_ZC writer substitutes the
    // socket write of the same framed buffer, so the bytes the peer receives
    // must be identical WITH (`Enabled`) and WITHOUT (`Auto`) `--zero-copy`.
    // Drives a payload larger than the SEND_ZC dispatch threshold through
    // `daemon_socket_writer` over a loopback TCP pair under each policy and
    // asserts the received bytes match exactly. On a kernel without SEND_ZC
    // both policies use the plain writer, which is the byte-identical baseline;
    // on a SEND_ZC kernel the `Enabled` bytes must still match `Auto` exactly.
    #[cfg(unix)]
    #[test]
    fn daemon_socket_writer_is_byte_identical_across_policies() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        fn transcript(policy: fast_io::ZeroCopyPolicy, payload: &[u8]) -> Vec<u8> {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("local addr");
            let payload_owned = payload.to_vec();

            let sender = std::thread::spawn(move || {
                let write_stream = TcpStream::connect(addr).expect("connect");
                let mut writer = daemon_socket_writer(write_stream, policy);
                writer.write_all(&payload_owned).expect("write payload");
                writer.flush().expect("flush");
                // Drop closes the socket so the reader sees EOF.
            });

            let (mut peer, _) = listener.accept().expect("accept");
            let mut received = Vec::new();
            peer.read_to_end(&mut received).expect("read to end");
            sender.join().expect("sender thread");
            received
        }

        // 256 KiB - above the 16 KiB / 4 KiB SEND_ZC dispatch thresholds and
        // the 64 KiB frame buffer, so the write spans multiple submissions.
        let payload: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();

        let auto = transcript(fast_io::ZeroCopyPolicy::Auto, &payload);
        let enabled = transcript(fast_io::ZeroCopyPolicy::Enabled, &payload);

        assert_eq!(auto, payload, "default (Auto) transcript must equal input");
        assert_eq!(
            enabled, auto,
            "SEND_ZC (--zero-copy) transcript must be byte-identical to the default path"
        );
    }

    // UTS-15.g: the daemon arg parser must fail loud on a client-only batch
    // flag instead of silently dropping it. Upstream rsync at
    // `options.c:1444-1449` emits `rsync: <BAD>: <err> (in daemon mode)` and
    // exits `RERR_SYNTAX` via `daemon_error:` (options.c:1464-1466). We
    // mirror that surface: the parser returns the offending arg so the
    // caller can write an `@ERROR` frame and reject the connection.
    #[test]
    fn apply_long_form_args_reports_write_batch_kv_as_unknown() {
        let args = vec![
            "--server".to_owned(),
            "--write-batch=/tmp/bad.batch".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let offender = apply_long_form_args(&args, &mut config);
        assert_eq!(offender.as_deref(), Some("--write-batch=/tmp/bad.batch"));
    }

    #[test]
    fn apply_long_form_args_reports_read_batch_kv_as_unknown() {
        let args = vec![
            "--server".to_owned(),
            "--read-batch=/tmp/in.batch".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let offender = apply_long_form_args(&args, &mut config);
        assert_eq!(offender.as_deref(), Some("--read-batch=/tmp/in.batch"));
    }

    #[test]
    fn apply_long_form_args_reports_only_write_batch_kv_as_unknown() {
        let args = vec![
            "--server".to_owned(),
            "--only-write-batch=/tmp/dry.batch".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let offender = apply_long_form_args(&args, &mut config);
        assert_eq!(offender.as_deref(), Some("--only-write-batch=/tmp/dry.batch"));
    }

    // Recognised client args do NOT produce the unknown-arg signal. This
    // guards against regressions that would mis-classify everyday daemon
    // argv such as `--delete`, `--temp-dir=`, and reference-directory
    // values as unknown.
    #[test]
    fn apply_long_form_args_recognised_args_do_not_report_unknown() {
        let args = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "-logDtprz".to_owned(),
            "--delete-before".to_owned(),
            "--max-delete=10".to_owned(),
            "--temp-dir=/staging".to_owned(),
            "--link-dest=/prev".to_owned(),
            ".".to_owned(),
            "module/sub".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let offender = apply_long_form_args(&args, &mut config);
        assert!(offender.is_none(), "no unknown should be reported: {offender:?}");
    }

    // Positional path arguments past the `.` separator must not be
    // mis-classified as unknown options - they are dispatched through
    // upstream's `glob_expand_module()` (util1.c:804), not popt.
    #[test]
    fn apply_long_form_args_positional_paths_are_not_classified() {
        let args = vec![
            "--server".to_owned(),
            "-logDtpr".to_owned(),
            ".".to_owned(),
            "module/--write-batch=foo.bin".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let offender = apply_long_form_args(&args, &mut config);
        assert!(offender.is_none(), "positional paths must not flag: {offender:?}");
    }

    #[test]
    fn parse_daemon_dont_compress_glob_suffixes() {
        let list = parse_daemon_dont_compress("*.gz *.zip *.jpg").expect("should parse");
        assert!(list.matches_path(Path::new("archive.gz")));
        assert!(list.matches_path(Path::new("bundle.zip")));
        assert!(list.matches_path(Path::new("photo.jpg")));
        assert!(!list.matches_path(Path::new("notes.txt")));
    }

    #[test]
    fn parse_daemon_dont_compress_bare_suffixes() {
        let list = parse_daemon_dont_compress("gz zip").expect("should parse");
        assert!(list.matches_path(Path::new("archive.gz")));
        assert!(list.matches_path(Path::new("bundle.zip")));
    }

    #[test]
    fn parse_daemon_dont_compress_empty_returns_none() {
        assert!(parse_daemon_dont_compress("").is_none());
        assert!(parse_daemon_dont_compress("   ").is_none());
    }

    #[test]
    fn parse_daemon_dont_compress_case_insensitive() {
        let list = parse_daemon_dont_compress("*.GZ *.ZIP").expect("should parse");
        assert!(list.matches_path(Path::new("archive.gz")));
        assert!(list.matches_path(Path::new("ARCHIVE.GZ")));
    }

    #[test]
    fn parse_daemon_dont_compress_mixed_formats() {
        let list = parse_daemon_dont_compress("*.gz mp3 .bz2").expect("should parse");
        assert!(list.matches_path(Path::new("file.gz")));
        assert!(list.matches_path(Path::new("song.mp3")));
        assert!(list.matches_path(Path::new("archive.bz2")));
    }

    // WHY: upstream token.c:206-211 treats a bare `*` in the dont-compress match
    // list as the whole-stream store signal (not a per-file suffix). A normal
    // suffix list must not be mistaken for it, or ordinary transfers would lose
    // compression.
    #[test]
    fn dont_compress_bare_star_is_match_all() {
        assert!(dont_compress_is_match_all("*"));
        assert!(dont_compress_is_match_all("*.gz *"));
        assert!(!dont_compress_is_match_all("*.gz *.zip"));
        assert!(!dont_compress_is_match_all("gz"));
        assert!(!dont_compress_is_match_all(""));
    }


    fn test_module_with_defaults() -> ModuleRuntime {
        ModuleRuntime::from(ModuleDefinition::default())
    }

    #[test]
    fn build_daemon_filter_rules_empty_module() {
        let module = test_module_with_defaults();
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn build_daemon_filter_rules_exclude_patterns() {
        let module = ModuleRuntime::from(ModuleDefinition {
            exclude: vec!["*.tmp".to_string(), "*.bak".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern, "*.tmp");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[1].pattern, "*.bak");
        assert_eq!(rules[1].rule_type, protocol::filters::RuleType::Exclude);
    }

    #[test]
    fn build_daemon_filter_rules_include_patterns() {
        let module = ModuleRuntime::from(ModuleDefinition {
            include: vec!["*.txt".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "*.txt");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Include);
    }

    #[test]
    fn build_daemon_filter_rules_filter_syntax() {
        let module = ModuleRuntime::from(ModuleDefinition {
            filter: vec!["- *.log".to_string(), "+ *.rs".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern, "*.log");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[1].pattern, "*.rs");
        assert_eq!(rules[1].rule_type, protocol::filters::RuleType::Include);
    }

    #[test]
    fn build_daemon_filter_rules_word_split_exclude() {
        let module = ModuleRuntime::from(ModuleDefinition {
            exclude: vec!["*.tmp *.bak *.log".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern, "*.tmp");
        assert_eq!(rules[1].pattern, "*.bak");
        assert_eq!(rules[2].pattern, "*.log");
    }

    #[test]
    fn build_daemon_filter_rules_exclude_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let exclude_file = dir.path().join("excludes.txt");
        fs::write(&exclude_file, "*.tmp\n*.bak\n# comment\n\n*.log\n").unwrap();

        let module = ModuleRuntime::from(ModuleDefinition {
            exclude_from: Some(exclude_file),
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern, "*.tmp");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[1].pattern, "*.bak");
        assert_eq!(rules[2].pattern, "*.log");
    }

    #[test]
    fn build_daemon_filter_rules_include_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let include_file = dir.path().join("includes.txt");
        fs::write(&include_file, "*.rs\n; semicolon comment\n*.toml\n").unwrap();

        let module = ModuleRuntime::from(ModuleDefinition {
            include_from: Some(include_file),
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern, "*.rs");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rules[1].pattern, "*.toml");
        assert_eq!(rules[1].rule_type, protocol::filters::RuleType::Include);
    }

    #[test]
    fn build_daemon_filter_rules_missing_file_returns_error() {
        let module = ModuleRuntime::from(ModuleDefinition {
            exclude_from: Some(PathBuf::from("/nonexistent/excludes.txt")),
            ..Default::default()
        });
        let result = build_daemon_filter_rules(&module);
        assert!(result.is_err());
    }

    #[test]
    fn build_daemon_filter_rules_ordering_filter_include_exclude_files() {
        let dir = tempfile::tempdir().unwrap();
        let include_file = dir.path().join("includes.txt");
        fs::write(&include_file, "*.rs\n").unwrap();
        let exclude_file = dir.path().join("excludes.txt");
        fs::write(&exclude_file, "*.log\n").unwrap();

        let module = ModuleRuntime::from(ModuleDefinition {
            filter: vec!["- *.tmp".to_string()],
            include: vec!["*.toml".to_string()],
            exclude: vec!["*.bak".to_string()],
            include_from: Some(include_file),
            exclude_from: Some(exclude_file),
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();

        // upstream: clientserver.c:874-893 - order is:
        // filter, include_from, include, exclude_from, exclude
        assert_eq!(rules.len(), 5);
        assert_eq!(rules[0].pattern, "*.tmp");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[1].pattern, "*.rs");
        assert_eq!(rules[1].rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rules[2].pattern, "*.toml");
        assert_eq!(rules[2].rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rules[3].pattern, "*.log");
        assert_eq!(rules[3].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[4].pattern, "*.bak");
        assert_eq!(rules[4].rule_type, protocol::filters::RuleType::Exclude);
    }

    #[test]
    fn build_daemon_filter_rules_anchored_pattern() {
        let module = ModuleRuntime::from(ModuleDefinition {
            exclude: vec!["/secret".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "/secret");
        assert!(rules[0].anchored);
    }

    #[test]
    fn build_daemon_filter_rules_directory_only_exclude_gets_dir2wild3() {
        // upstream: exclude.c:211-217 - XFLG_DIR2WILD3 converts directory-only
        // exclude patterns from "dir/" to "dir/***" and clears FILTRULE_DIRECTORY.
        let module = ModuleRuntime::from(ModuleDefinition {
            exclude: vec!["cache/".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "cache/***");
        assert!(!rules[0].directory_only);
    }

    #[test]
    fn build_daemon_filter_rules_directory_only_include_keeps_slash() {
        // upstream: exclude.c:213 - DIR2WILD3 only applies to exclude rules,
        // not include rules (BITS_SETnUNSET(FILTRULE_DIRECTORY, FILTRULE_INCLUDE)).
        let module = ModuleRuntime::from(ModuleDefinition {
            include: vec!["cache/".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "cache/");
        assert!(rules[0].directory_only);
    }

    #[test]
    fn build_daemon_filter_rules_filter_directive_with_keyword() {
        // This is the exact case from the interop test: filter = exclude *.bak
        let module = ModuleRuntime::from(ModuleDefinition {
            filter: vec!["exclude *.bak".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[0].pattern, "*.bak");
    }

    #[test]
    fn build_daemon_filter_rules_mixed_directives_with_keywords() {
        // Simulates: exclude = *.tmp, exclude = *.log, filter = exclude *.bak
        // Upstream order: filter first, then include, then exclude.
        let module = ModuleRuntime::from(ModuleDefinition {
            exclude: vec!["*.tmp".to_string(), "*.log".to_string()],
            filter: vec!["exclude *.bak".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 3);
        // filter rules are processed first (upstream: clientserver.c:874)
        assert_eq!(rules[0].pattern, "*.bak");
        // then excludes (upstream: clientserver.c:891)
        assert_eq!(rules[1].pattern, "*.tmp");
        assert_eq!(rules[2].pattern, "*.log");
        // All should be excludes
        for rule in &rules {
            assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        }
    }

    #[test]
    fn build_daemon_filter_rules_from_file_skips_comments_and_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("patterns.txt");
        fs::write(
            &file,
            "# header comment\n\n  \n*.tmp\n; another comment\n*.bak\n\n",
        )
        .unwrap();

        let module = ModuleRuntime::from(ModuleDefinition {
            exclude_from: Some(file),
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern, "*.tmp");
        assert_eq!(rules[1].pattern, "*.bak");
    }


    #[test]
    fn build_pattern_rule_exclude() {
        let rule = build_pattern_rule("*.tmp", false);
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rule.pattern, "*.tmp");
        assert!(!rule.anchored);
        assert!(!rule.directory_only);
    }

    #[test]
    fn build_pattern_rule_include() {
        let rule = build_pattern_rule("*.rs", true);
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rule.pattern, "*.rs");
    }

    #[test]
    fn build_pattern_rule_anchored() {
        let rule = build_pattern_rule("/etc", false);
        assert!(rule.anchored);
        assert_eq!(rule.pattern, "/etc");
    }

    #[test]
    fn build_pattern_rule_doublestar_prefix_stays_unanchored() {
        // A `**`-prefixed daemon exclude contains a slash but must NOT be
        // anchored: upstream sets WILD2_PREFIX independently of ABS_PATH, and
        // anchoring would prepend `/` (-> `/**/*.o`) and stop `**/*.o` from
        // matching a root-level `build.o`. Regression for the
        // daemon-filter-doublestar interop test.
        let rule = build_pattern_rule("**/*.o", false);
        assert!(!rule.anchored, "`**/*.o` must stay unanchored");
        assert_eq!(rule.pattern, "**/*.o");

        // A slash-containing pattern that does NOT start with `**` is still
        // anchored (XFLG_ABS_IF_SLASH).
        let nested = build_pattern_rule("sub/file.o", false);
        assert!(nested.anchored, "`sub/file.o` is anchored by ABS_IF_SLASH");
    }

    #[test]
    fn build_pattern_rule_directory_only_exclude_dir2wild3() {
        // upstream: exclude.c:211-217 - XFLG_DIR2WILD3 transforms dir/ to dir/***
        let rule = build_pattern_rule("build/", false);
        assert!(!rule.directory_only);
        assert_eq!(rule.pattern, "build/***");
    }

    #[test]
    fn build_pattern_rule_directory_only_include_preserved() {
        let rule = build_pattern_rule("build/", true);
        assert!(rule.directory_only);
        assert_eq!(rule.pattern, "build/");
    }

    #[test]
    fn pattern_leading_slash_is_anchored() {
        let rule = build_pattern_rule("/foo", false);
        assert!(rule.anchored);
    }

    #[test]
    fn pattern_no_slash_is_not_anchored() {
        let rule = build_pattern_rule("*.txt", false);
        assert!(!rule.anchored);
    }

    #[test]
    fn pattern_embedded_slash_is_anchored() {
        // upstream: exclude.c:200-202 - XFLG_ABS_IF_SLASH anchors patterns
        // with any slash, not just leading slash
        let rule = build_pattern_rule("subdir/file.txt", false);
        assert!(rule.anchored);
    }

    #[test]
    fn pattern_deep_path_is_anchored() {
        let rule = build_pattern_rule("a/b/c", false);
        assert!(rule.anchored);
    }

    #[test]
    fn directory_exclude_gets_wild3() {
        let rule = build_pattern_rule("foo/", false);
        assert!(rule.anchored); // has embedded '/'
        assert!(!rule.directory_only); // cleared by DIR2WILD3
        assert!(rule.pattern.ends_with("/***"));
    }

    #[test]
    fn directory_include_keeps_directory_flag() {
        let rule = build_pattern_rule("bar/", true);
        assert!(rule.directory_only);
    }

    #[test]
    fn include_with_embedded_slash_is_anchored() {
        let rule = build_pattern_rule("src/main.rs", true);
        assert!(rule.anchored);
    }

    #[test]
    fn parse_daemon_filter_token_exclude() {
        let rule = parse_daemon_filter_token("- *.tmp").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rule.pattern, "*.tmp");
    }

    #[test]
    fn parse_daemon_filter_token_include() {
        let rule = parse_daemon_filter_token("+ *.rs").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rule.pattern, "*.rs");
    }

    #[test]
    fn parse_daemon_filter_token_bare_pattern_defaults_to_exclude() {
        let rule = parse_daemon_filter_token("*.bak").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rule.pattern, "*.bak");
    }

    #[test]
    fn parse_daemon_filter_token_empty_returns_none() {
        assert!(parse_daemon_filter_token("").is_none());
    }

    #[test]
    fn parse_daemon_filter_token_prefix_only_returns_none() {
        assert!(parse_daemon_filter_token("-").is_none());
        assert!(parse_daemon_filter_token("+").is_none());
    }


    #[test]
    fn parse_daemon_filter_token_exclude_keyword() {
        let rule = parse_daemon_filter_token("exclude *.bak").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rule.pattern, "*.bak");
    }

    #[test]
    fn parse_daemon_filter_token_exclude_keyword_comma_sep() {
        // upstream: RULE_STRCMP accepts comma as separator
        let rule = parse_daemon_filter_token("exclude,*.bak").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rule.pattern, "*.bak");
    }

    #[test]
    fn parse_daemon_filter_token_include_keyword() {
        let rule = parse_daemon_filter_token("include *.rs").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rule.pattern, "*.rs");
    }

    #[test]
    fn parse_daemon_filter_token_hide_keyword() {
        // upstream: hide -> sender-side exclude
        let rule = parse_daemon_filter_token("hide *.secret").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rule.pattern, "*.secret");
        assert!(rule.sender_side);
        assert!(!rule.receiver_side);
    }

    #[test]
    fn parse_daemon_filter_token_show_keyword() {
        // upstream: show -> sender-side include
        let rule = parse_daemon_filter_token("show *.pub").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rule.pattern, "*.pub");
        assert!(rule.sender_side);
        assert!(!rule.receiver_side);
    }

    #[test]
    fn parse_daemon_filter_token_protect_keyword() {
        // upstream: protect -> receiver-side exclude
        let rule = parse_daemon_filter_token("protect *.conf").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rule.pattern, "*.conf");
        assert!(!rule.sender_side);
        assert!(rule.receiver_side);
    }

    #[test]
    fn parse_daemon_filter_token_risk_keyword() {
        // upstream: risk -> receiver-side include
        let rule = parse_daemon_filter_token("risk *.tmp").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rule.pattern, "*.tmp");
        assert!(!rule.sender_side);
        assert!(rule.receiver_side);
    }

    #[test]
    fn parse_daemon_filter_token_clear_keyword() {
        let rule = parse_daemon_filter_token("clear").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Clear);
        assert!(rule.pattern.is_empty());
    }

    #[test]
    fn parse_daemon_filter_token_keyword_not_partial_match() {
        // "excluder" should NOT match "exclude" keyword - treated as bare pattern
        let rule = parse_daemon_filter_token("excluder *.tmp").unwrap();
        assert_eq!(rule.rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rule.pattern, "excluder *.tmp");
    }

    #[test]
    fn parse_daemon_filter_token_keyword_empty_pattern_returns_none() {
        assert!(parse_daemon_filter_token("exclude").is_none());
        assert!(parse_daemon_filter_token("include ").is_none());
    }


    #[test]
    fn strip_keyword_prefix_space_separator() {
        assert_eq!(strip_keyword_prefix("exclude *.tmp", "exclude"), Some("*.tmp"));
    }

    #[test]
    fn strip_keyword_prefix_comma_separator() {
        assert_eq!(strip_keyword_prefix("exclude,*.tmp", "exclude"), Some("*.tmp"));
    }

    #[test]
    fn strip_keyword_prefix_no_separator() {
        // "excluder" should not match "exclude"
        assert_eq!(strip_keyword_prefix("excluder *.tmp", "exclude"), None);
    }

    #[test]
    fn strip_keyword_prefix_exact_keyword_no_pattern() {
        assert_eq!(strip_keyword_prefix("exclude", "exclude"), Some(""));
    }

    #[test]
    fn strip_keyword_prefix_no_match() {
        assert_eq!(strip_keyword_prefix("include *.tmp", "exclude"), None);
    }


    #[test]
    fn read_patterns_from_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("patterns.txt");
        fs::write(&file, "*.tmp\n*.bak\n").unwrap();

        let patterns = read_patterns_from_file(&file).unwrap();
        assert_eq!(patterns, vec!["*.tmp", "*.bak"]);
    }

    #[test]
    fn read_patterns_from_file_skips_comments() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("patterns.txt");
        fs::write(&file, "# comment\n*.tmp\n; another\n*.bak\n").unwrap();

        let patterns = read_patterns_from_file(&file).unwrap();
        assert_eq!(patterns, vec!["*.tmp", "*.bak"]);
    }

    #[test]
    fn read_patterns_from_file_skips_empty_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("patterns.txt");
        fs::write(&file, "\n*.tmp\n  \n\n*.bak\n").unwrap();

        let patterns = read_patterns_from_file(&file).unwrap();
        assert_eq!(patterns, vec!["*.tmp", "*.bak"]);
    }

    #[test]
    fn read_patterns_from_file_missing_file() {
        let result = read_patterns_from_file(Path::new("/nonexistent/file.txt"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to read filter file"));
    }

    #[test]
    fn secluded_args_flag_standalone() {
        let args: Vec<String> = vec!["--server", "-s", "."]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(has_secluded_args_flag(&args));
    }

    #[test]
    fn secluded_args_flag_bundled_compact() {
        let args: Vec<String> = vec!["--server", "-logDtprs", "."]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(has_secluded_args_flag(&args));
    }

    #[test]
    fn secluded_args_flag_long_protect_args() {
        let args: Vec<String> = vec!["--server", "--protect-args", "."]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(has_secluded_args_flag(&args));
    }

    #[test]
    fn secluded_args_flag_long_secluded_args() {
        let args: Vec<String> = vec!["--server", "--secluded-args", "."]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(has_secluded_args_flag(&args));
    }

    #[test]
    fn secluded_args_flag_absent() {
        let args: Vec<String> = vec!["--server", "-logDtpr", "."]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(!has_secluded_args_flag(&args));
    }

    #[test]
    fn secluded_args_flag_not_in_long_option() {
        // `--some-option` should not match even if it contains 's'
        let args: Vec<String> = vec!["--server", "--sender", "."]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(!has_secluded_args_flag(&args));
    }

    #[test]
    fn secluded_args_flag_empty_args() {
        let args: Vec<String> = vec![];
        assert!(!has_secluded_args_flag(&args));
    }

    #[test]
    fn secluded_args_not_in_capability_string() {
        // The 's' in `.iLsfxCIvu` is SYMLINK_ICONV, not secluded-args.
        // `-e` consumes the rest as its parameter, so scanning must stop at 'e'.
        // upstream: options.c uses popt which knows `-e` takes an argument.
        let args: Vec<String> = vec!["--server", "-vlogDtpre.iLsfxCIvu", "."]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(!has_secluded_args_flag(&args));
    }

    #[test]
    fn secluded_args_before_e_in_compact_flags() {
        // `-s` appearing before `-e` in compact flags should still be detected.
        let args: Vec<String> = vec!["--server", "-vlogDtprse.iLfxCIvu", "."]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(has_secluded_args_flag(&args));
    }


    #[test]
    fn apply_long_form_args_parses_backup_dir_two_arg() {
        let args = vec![
            "--server".to_owned(),
            "--backup-dir".to_owned(),
            ".backups".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.backup_dir.as_deref(), Some(".backups"));
        assert!(config.flags.backup);
    }

    #[test]
    fn apply_long_form_args_parses_backup_dir_equals() {
        let args = vec![
            "--server".to_owned(),
            "--backup-dir=.backups".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.backup_dir.as_deref(), Some(".backups"));
        assert!(config.flags.backup);
    }

    #[test]
    fn apply_long_form_args_backup_dir_effective_suffix_is_empty() {
        // upstream: options.c:2278-2279 - when --backup-dir is set and no
        // explicit --suffix is sent, the default suffix is "" (empty).
        let args = vec![
            "--server".to_owned(),
            "--backup-dir".to_owned(),
            ".backups".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.effective_backup_suffix(), "");
    }

    #[test]
    fn apply_long_form_args_backup_dir_with_explicit_suffix() {
        let args = vec![
            "--server".to_owned(),
            "--backup-dir".to_owned(),
            ".backups".to_owned(),
            "--suffix".to_owned(),
            ".old".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert_eq!(config.backup_dir.as_deref(), Some(".backups"));
        assert_eq!(config.effective_backup_suffix(), ".old");
    }

    #[test]
    fn split_filter_tokens_single_exclude() {
        let tokens = split_filter_tokens("- *.tmp");
        assert_eq!(tokens, vec!["- *.tmp"]);
    }

    #[test]
    fn split_filter_tokens_single_include() {
        let tokens = split_filter_tokens("+ *.rs");
        assert_eq!(tokens, vec!["+ *.rs"]);
    }

    #[test]
    fn split_filter_tokens_multiple_rules() {
        let tokens = split_filter_tokens("+ *.txt + *.rs + */ - *");
        assert_eq!(tokens, vec!["+ *.txt", "+ *.rs", "+ */", "- *"]);
    }

    #[test]
    fn split_filter_tokens_mixed_include_exclude() {
        let tokens = split_filter_tokens("+ important.log + .keep.tmp - *.log - *.tmp");
        assert_eq!(
            tokens,
            vec!["+ important.log", "+ .keep.tmp", "- *.log", "- *.tmp"]
        );
    }

    #[test]
    fn split_filter_tokens_excludes_only() {
        let tokens = split_filter_tokens("- *.tmp - *.bak - *.cache");
        assert_eq!(tokens, vec!["- *.tmp", "- *.bak", "- *.cache"]);
    }

    #[test]
    fn split_filter_tokens_empty() {
        let tokens = split_filter_tokens("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn split_filter_tokens_whitespace_only() {
        let tokens = split_filter_tokens("   ");
        assert!(tokens.is_empty());
    }

    #[test]
    fn split_filter_tokens_keyword_rules() {
        let tokens = split_filter_tokens("exclude *.tmp include *.rs");
        assert_eq!(tokens, vec!["exclude *.tmp", "include *.rs"]);
    }

    #[test]
    fn split_filter_tokens_bare_pattern() {
        let tokens = split_filter_tokens("*.bak");
        assert_eq!(tokens, vec!["*.bak"]);
    }

    #[test]
    fn build_daemon_filter_rules_filter_word_split_include_exclude() {
        // Matches the test_daemon_filter_include_exclude_star interop test
        let module = ModuleRuntime::from(ModuleDefinition {
            filter: vec!["+ *.txt + *.rs + */ - *".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].pattern, "*.txt");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rules[1].pattern, "*.rs");
        assert_eq!(rules[1].rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rules[2].pattern, "*/");
        assert_eq!(rules[2].rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rules[3].pattern, "*");
        assert_eq!(rules[3].rule_type, protocol::filters::RuleType::Exclude);
    }

    #[test]
    fn build_daemon_filter_rules_filter_word_split_excludes() {
        // Matches the test_daemon_filter_directive_types interop test
        let module = ModuleRuntime::from(ModuleDefinition {
            filter: vec!["- *.tmp - *.bak - *.cache".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern, "*.tmp");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[1].pattern, "*.bak");
        assert_eq!(rules[1].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[2].pattern, "*.cache");
        assert_eq!(rules[2].rule_type, protocol::filters::RuleType::Exclude);
    }

    #[test]
    fn build_daemon_filter_rules_filter_word_split_overlapping() {
        // Matches the test_daemon_filter_overlapping_rules interop test
        let module = ModuleRuntime::from(ModuleDefinition {
            filter: vec!["+ important.log + .keep.tmp - *.log - *.tmp".to_string()],
            ..Default::default()
        });
        let rules = build_daemon_filter_rules(&module).unwrap();
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].pattern, "important.log");
        assert_eq!(rules[0].rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rules[1].pattern, ".keep.tmp");
        assert_eq!(rules[1].rule_type, protocol::filters::RuleType::Include);
        assert_eq!(rules[2].pattern, "*.log");
        assert_eq!(rules[2].rule_type, protocol::filters::RuleType::Exclude);
        assert_eq!(rules[3].pattern, "*.tmp");
        assert_eq!(rules[3].rule_type, protocol::filters::RuleType::Exclude);
    }

    // upstream: util1.c:813-814 (glob_expand_module) - parity tests for the
    // chdir-symlink-race fix that wires the client's positional dest through
    // to the receiver, instead of silently routing every write into the
    // module root.

    #[test]
    fn extract_module_relative_paths_strips_module_prefix() {
        let args = vec![
            "--server".to_owned(),
            "-vve.LsfxCIvu".to_owned(),
            ".".to_owned(),
            "upload/realdir/".to_owned(),
        ];
        let paths = extract_module_relative_paths(&args, "upload");
        assert_eq!(paths, vec!["realdir/".to_owned()]);
    }

    #[test]
    fn extract_module_relative_paths_handles_bare_module_arg() {
        let args = vec![
            "--server".to_owned(),
            "-vve.LsfxCIvu".to_owned(),
            ".".to_owned(),
            "upload/".to_owned(),
        ];
        let paths = extract_module_relative_paths(&args, "upload");
        assert_eq!(paths, vec!["".to_owned()]);
    }

    #[test]
    fn extract_module_relative_paths_returns_empty_without_dot() {
        // No dot separator means nothing positional was sent (e.g. a probe
        // request that exits before the file list).
        let args = vec!["--server".to_owned(), "-vve.LsfxCIvu".to_owned()];
        let paths = extract_module_relative_paths(&args, "upload");
        assert!(paths.is_empty());
    }

    #[test]
    fn extract_module_relative_paths_does_not_chop_sibling_prefix() {
        // The module is "upload"; an arg starting with "uploads/" must NOT
        // be stripped - that arg belongs to a different module sharing a
        // string prefix and stripping it would mis-route the request.
        let args = vec![".".to_owned(), "uploads/x/".to_owned()];
        let paths = extract_module_relative_paths(&args, "upload");
        assert_eq!(paths, vec!["uploads/x/".to_owned()]);
    }

    #[test]
    fn resolve_receiver_dest_joins_subpath_with_module_root() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/realdir/".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload").expect("valid dest");
        assert_eq!(dest, std::path::Path::new("/srv/upload/realdir/"));
    }

    #[test]
    fn resolve_receiver_dest_falls_back_to_module_root_for_bare_module() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload").expect("valid dest");
        assert_eq!(dest, std::path::Path::new("/srv/upload"));
    }

    #[test]
    fn resolve_receiver_dest_falls_back_to_module_root_when_no_positional() {
        let module_path = std::path::Path::new("/srv/upload");
        let args: Vec<String> = vec![];
        let dest = resolve_receiver_dest(module_path, &args, "upload").expect("valid dest");
        assert_eq!(dest, std::path::Path::new("/srv/upload"));
    }

    #[test]
    fn resolve_receiver_dest_uses_last_positional_for_multi_arg_push() {
        // The receiver's destination is the LAST positional - everything
        // earlier is a source path the sender is reading from.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![
            ".".to_owned(),
            "upload/srcA/".to_owned(),
            "upload/srcB/".to_owned(),
            "upload/destdir/".to_owned(),
        ];
        let dest = resolve_receiver_dest(module_path, &args, "upload").expect("valid dest");
        assert_eq!(dest, std::path::Path::new("/srv/upload/destdir/"));
    }

    #[test]
    fn resolve_receiver_dest_rejoins_absolute_path_under_module_root() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "/etc/passwd".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload").expect("valid dest");
        // Absolute path is forced under the module root - no escape.
        assert_eq!(dest, std::path::Path::new("/srv/upload/etc/passwd"));
    }

    #[test]
    fn resolve_receiver_dest_rejects_parent_dir_traversal() {
        // Defense-in-depth: a `..` segment in the receiver destination is
        // rejected up front, symmetric to `resolve_sender_sources`. Every
        // valid (dotdot-free) tail is unaffected; this only fails the escape.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/../../etc/passwd".to_owned()];
        assert!(resolve_receiver_dest(module_path, &args, "upload").is_none());
    }

    // URV-5.b.REOPEN: classify_client_path_against_module is the pure helper
    // that decides whether a raw client-supplied path goes into the Landlock
    // allowlist (Ok(Some(canonical))), is silently accepted as relative
    // (Ok(None)), or is rejected (Err(())). These tests pin the trust
    // boundary so widening the allowlist cannot accidentally admit
    // out-of-module paths.

    #[test]
    fn classify_client_path_relative_path_returns_none() {
        let module_root = std::path::Path::new("/srv/module");
        let result = classify_client_path_against_module(".rsync-tmp", module_root);
        // Relative paths resolve under the module root or chroot cwd; they
        // never need an explicit allowlist entry.
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn classify_client_path_in_module_absolute_is_admitted() {
        // Use the OS tempdir so the canonicalisation actually succeeds on
        // the platform running the test, then probe a sub-path that we
        // construct beneath it (which need not exist).
        let module = tempfile::TempDir::new().expect("module tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let in_module = module_root.join("alt-basis");
        let raw = in_module.to_string_lossy().into_owned();
        let result = classify_client_path_against_module(&raw, &module_root);
        match result {
            Ok(Some(p)) => assert!(
                p.starts_with(&module_root),
                "admitted path '{}' must start with module root '{}'",
                p.display(),
                module_root.display(),
            ),
            other => panic!("expected admitted in-module path, got {other:?}"),
        }
    }

    #[test]
    fn classify_client_path_out_of_module_absolute_is_rejected() {
        let module = tempfile::TempDir::new().expect("module tempdir");
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let outside_root = outside.path().canonicalize().expect("canonicalise outside");
        let raw = outside_root.to_string_lossy().into_owned();
        let result = classify_client_path_against_module(&raw, &module_root);
        // The whole point of SEC-1.p: an attacker-supplied prefix that
        // escapes the module root must be rejected, never admitted.
        assert!(matches!(result, Err(())));
    }

    #[test]
    fn confine_basis_drops_absolute_out_of_module() {
        // CI-MASTER-INTEROP regression pin (standalone:link-dest /
        // standalone:copy-dest): the upstream interop harness sends an
        // absolute `--link-dest` that canonicalises *outside* the module
        // root (a sibling path `<module>/../linkdest-ref-daemon`). The
        // daemon must silently drop the basis so the receiver re-transfers
        // instead of aborting with `@ERROR` - aborting broke the standalone
        // suite on master. upstream `main.c:841 check_alt_basis_dirs` warns
        // on a missing/out-of-tree basis but never aborts.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let outside_root = outside.path().canonicalize().expect("canonicalise outside");

        assert!(
            confine_basis_under_module(&outside_root, &module_root, &module_root).is_none(),
            "absolute out-of-module basis must be dropped",
        );
    }

    #[test]
    fn confine_basis_accepts_absolute_in_module() {
        // Companion to the drop-out-of-module pin: an absolute path that
        // canonicalises *inside* the module root must survive so legitimate
        // operator-permitted snapshots (e.g. `<module>/snap/01`) still
        // hard-link instead of re-transferring.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let in_module = module_root.join("snap");
        std::fs::create_dir(&in_module).expect("create in-module snap dir");

        let resolved = confine_basis_under_module(&in_module, &module_root, &module_root)
            .expect("in-module basis must be accepted");
        assert_eq!(resolved, in_module);
    }

    #[test]
    fn confine_basis_drops_absolute_dotdot_escape_to_sibling() {
        // The exact failure shape from the CI standalone:link-dest fixture:
        // the client sends `--link-dest=<module>/../linkdest-ref-daemon`,
        // which canonicalises to a sibling of the module root. Must be
        // silently dropped.
        let parent = tempfile::TempDir::new().expect("parent tempdir");
        let module_root = parent.path().join("linkdest-dest");
        std::fs::create_dir(&module_root).expect("create module root");
        let sibling = parent.path().join("linkdest-ref-daemon");
        std::fs::create_dir(&sibling).expect("create sibling");
        let module_root = module_root.canonicalize().expect("canonicalise module");

        // Lexical escape via `..` that resolves to a real sibling on disk.
        let escape = module_root.join("..").join("linkdest-ref-daemon");
        assert!(
            confine_basis_under_module(&escape, &module_root, &module_root).is_none(),
            "absolute `..` escape to sibling must be dropped (was @ERROR pre-fix)",
        );
    }

    #[test]
    fn confine_basis_joins_relative_under_resolve_base() {
        // Relative basis paths still resolve under the receiver's dest dir
        // (the `resolve_base`), matching upstream `main.c:1199-1206`
        // post-`get_local_name` chdir behaviour. This pins the legacy
        // relative branch so the absolute-path extension doesn't regress it.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let dest = module_root.join("00");
        std::fs::create_dir(&dest).expect("create dest 00");
        let sibling = module_root.join("01");
        std::fs::create_dir(&sibling).expect("create sibling 01");

        let resolved = confine_basis_under_module(
            std::path::Path::new("../01"),
            &dest,
            &module_root,
        )
        .expect("relative climb to in-module sibling must be accepted");
        // Lexically normalised: dest/../01 -> module_root/01.
        assert_eq!(resolved, module_root.join("01"));
    }

    #[test]
    fn classify_client_path_existing_dotdot_escape_is_rejected() {
        // `..` traversal against an *existing* path canonicalises to the
        // resolved target; if the target is outside the module root, the
        // helper rejects it. (URV-5.a / #3617 separately covers the
        // non-existent `..` escape via `RESOLVE_BENEATH`; this test pins
        // the existing-path branch the widening relies on.)
        let module = tempfile::TempDir::new().expect("module tempdir");
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let outside_root = outside.path().canonicalize().expect("canonicalise outside");
        // Build a traversal that canonicalises out of the module tree by
        // walking up to the shared tempdir parent and back down into the
        // sibling temp directory.
        let escape = format!(
            "{}/../{}",
            module_root.display(),
            outside_root
                .file_name()
                .expect("outside basename")
                .to_string_lossy(),
        );
        let result = classify_client_path_against_module(&escape, &module_root);
        assert!(matches!(result, Err(())));
    }

    // UTS-NEXTEST-EDGE.m link-dest module-escape security pins.
    //
    // Ports the upstream `alt-dest-symlink-race.test` / `link-dest-module-
    // escape` security scenario into the daemon's path-validation invariant.
    // Upstream's defence is `secure_relative_open()` at receiver basis-lookup
    // time; oc-rsync's daemon-side defence is `confine_basis_under_module`,
    // which drops the basis before the request ever reaches the receiver.
    //
    // Behavioural divergence from the upstream test: upstream's daemon never
    // emits a literal "outside the module" `@ERROR` for these scenarios. Its
    // `util1.c:1035 sanitize_path` collapses `..` against the module root
    // depth (rewriting the path under the module) and `main.c:841
    // check_alt_basis_dirs` only warns when the resulting basis is missing.
    // PR #5778 aligned the oc-rsync daemon with that contract by switching
    // from a hard `@ERROR` reject to a silent drop. These tests pin the
    // silent-drop contract so a future regression to either the old
    // `@ERROR` reject path or to admitting the escape cannot ship.

    #[test]
    fn confine_basis_link_dest_relative_etc_passwd_escape_is_dropped() {
        // Negative scenario from the upstream link-dest-module-escape pin:
        // the client sends `--link-dest=../etc/passwd` from a dest under the
        // module root that has fewer path components than the lexical climb.
        // The lexical normalisation collapses `<dest>/../etc/passwd` past
        // the module root to `<module_parent>/etc/passwd`, which `starts_with
        // (module_root)` rejects. The basis must be dropped so the receiver
        // re-transfers rather than hard-linking from outside the module.
        let parent = tempfile::TempDir::new().expect("parent tempdir");
        let module_root = parent.path().join("upload");
        std::fs::create_dir(&module_root).expect("create module root");
        let module_root = module_root.canonicalize().expect("canonicalise module");
        // resolve_base is the module root itself (receiver dest = module
        // root for a bare-module push), so `../etc/passwd` climbs one level
        // above the module and lands on a sibling path.
        let resolve_base = module_root.clone();
        let escape = std::path::Path::new("../etc/passwd");

        assert!(
            confine_basis_under_module(escape, &resolve_base, &module_root).is_none(),
            "--link-dest=../etc/passwd must be dropped (relative climb past module root)",
        );
    }

    #[test]
    fn confine_basis_link_dest_relative_in_module_sibling_is_accepted() {
        // Positive control paired with the `../etc/passwd` negative case
        // above. A relative basis that resolves to an in-module sibling
        // must survive so operator-permitted snapshot layouts (e.g. the
        // upstream `dest/00 + --link-dest=../01` pattern from main.c:1199
        // -1206) still hard-link instead of re-transferring.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let dest = module_root.join("00");
        std::fs::create_dir(&dest).expect("create dest 00");
        let sibling = module_root.join("01");
        std::fs::create_dir(&sibling).expect("create sibling 01");

        let resolved = confine_basis_under_module(
            std::path::Path::new("../01"),
            &dest,
            &module_root,
        )
        .expect("relative in-module sibling basis must survive");
        assert_eq!(resolved, module_root.join("01"));
    }

    #[test]
    #[cfg(unix)]
    fn confine_basis_link_dest_in_module_symlink_to_outside_is_dropped() {
        // Ports the exact attack shape from upstream
        // `alt-dest-symlink-race.test`: an in-module symlink (`mod/cd ->
        // /outside`) used as a relative `--link-dest=cd` target. The
        // canonicalisation step in `confine_basis_under_module` follows the
        // symlink, finds the target outside the module root, and the
        // containment check rejects it. Without this defence the receiver
        // would hard-link the destination to attacker-readable files
        // outside the module (the rsync delta-rolling read-disclosure
        // primitive the upstream test guards against).
        let parent = tempfile::TempDir::new().expect("parent tempdir");
        let module_root = parent.path().join("upload");
        std::fs::create_dir(&module_root).expect("create module root");
        let outside = parent.path().join("outside");
        std::fs::create_dir(&outside).expect("create outside dir");
        let module_root = module_root.canonicalize().expect("canonicalise module");
        let outside = outside.canonicalize().expect("canonicalise outside");

        // Plant the attacker's symlink trap inside the module.
        let trap = module_root.join("cd");
        std::os::unix::fs::symlink(&outside, &trap).expect("plant in-module symlink trap");

        // Client sends `--link-dest=cd`; resolve_base is the module root
        // (the receiver dest for the bare-module push the upstream test
        // uses). The symlink resolution must be detected and the basis
        // dropped.
        assert!(
            confine_basis_under_module(
                std::path::Path::new("cd"),
                &module_root,
                &module_root,
            )
            .is_none(),
            "in-module symlink whose target escapes the module must be dropped \
             (upstream alt-dest-symlink-race attack shape)",
        );
    }

    #[test]
    fn confine_basis_link_dest_absolute_etc_passwd_is_dropped() {
        // Companion to the relative-path test above for the absolute form
        // the upstream test family also exercises (`--link-dest=/etc/passwd`).
        // The path canonicalises (or, when missing, falls through the
        // lexical branch) to a location outside the module root, so the
        // basis must be dropped.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        // `/etc/passwd` typically exists on Unix CI and macOS; on Windows
        // the canonical form lives under `C:\Windows\System32\...`. Use a
        // path under a sibling tempdir so the test is portable and never
        // depends on whether `/etc/passwd` exists or is readable in CI.
        let outside_parent = tempfile::TempDir::new().expect("outside tempdir");
        let outside = outside_parent
            .path()
            .canonicalize()
            .expect("canonicalise outside")
            .join("passwd");
        std::fs::write(&outside, b"root:x:0:0:root:/root:/bin/sh\n").expect("write outside file");

        assert!(
            confine_basis_under_module(&outside, &module_root, &module_root).is_none(),
            "absolute --link-dest pointing outside the module root must be dropped",
        );
    }

    #[test]
    fn resolve_sender_sources_returns_module_root_without_positional() {
        // upstream: clientserver.c:1073 - bare module request (no sub-path)
        // means the sender walks the module root directly. The trailing `/`
        // signals "transfer the module contents" so the engine's
        // non_relative_walk_base keeps base == path and the walk emits a
        // `.` entry with FLAG_TOP_DIR (upstream `flist.c:2312-2322`
        // `DOTDIR_NAME` branch). Without the trailing slash, the engine
        // would split on the last `/` and emit `upload`/`upload/...`
        // instead of `./...`.
        let module_path = std::path::Path::new("/srv/upload");
        let args: Vec<String> = vec![];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("bare module request must resolve");
        assert_eq!(sources, vec![std::path::PathBuf::from("/srv/upload/")]);
    }

    #[test]
    fn resolve_sender_sources_returns_module_root_for_empty_subpath() {
        // upstream: util1.c:813-814 - `module/` strips to "" after
        // glob_expand_module; the daemon sender should still walk the module
        // root and emit "." with FLAG_TOP_DIR. The trailing slash is the
        // engine-side `DOTDIR_NAME` signal (see the bare-module test).
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("empty sub-path must resolve");
        assert_eq!(sources, vec![std::path::PathBuf::from("/srv/upload/")]);
    }

    #[test]
    fn resolve_sender_sources_joins_single_file_subpath_with_module_root() {
        // upstream: flist.c:2338-2349 - a single-file sub-path positional is
        // joined with module_path so the sender walks exactly that one path
        // and the per-positional dir/fn split emits the basename.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/d1/d2/f2".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("file sub-path must resolve");
        assert_eq!(
            sources,
            vec![std::path::PathBuf::from("/srv/upload/d1/d2/f2")]
        );
    }

    #[test]
    fn resolve_sender_sources_preserves_trailing_slash_on_subdir() {
        // upstream: flist.c:2312-2322 - a trailing slash promotes the source
        // to DOTDIR_NAME; we must keep the slash intact so the sender's walk
        // emits "." with FLAG_TOP_DIR for the sub-directory's contents.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/d1/d2/".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("sub-dir trailing-slash path must resolve");
        let lossy: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(lossy, vec!["/srv/upload/d1/d2/".to_owned()]);
    }

    #[test]
    fn resolve_sender_sources_rejects_parent_dir_traversal() {
        // SEC-1.q defense-in-depth: a `..` segment escaping the module root
        // must be rejected at argv-resolution time so chroot-less daemons
        // cannot leak files via sub-path requests like `module/../etc/...`.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/../etc/passwd".to_owned()];
        assert!(resolve_sender_sources(module_path, &args, "upload").is_none());
    }

    #[test]
    fn resolve_sender_sources_rejects_mid_path_parent_dir() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/d1/../../secret".to_owned()];
        assert!(resolve_sender_sources(module_path, &args, "upload").is_none());
    }

    #[test]
    fn resolve_sender_sources_strips_leading_slash_before_join() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload//d1/d2/f2".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("leading-slash sub-path must resolve");
        assert_eq!(
            sources,
            vec![std::path::PathBuf::from("/srv/upload/d1/d2/f2")]
        );
    }

    // Glob expansion - upstream util1.c:804 glob_expand_module + util1.c:755
    // glob_expand. These tests cover the regression that surfaced as the
    // upstream `daemon` testsuite hanging on subtest 4 (`test-from/f*`):
    // without glob expansion the daemon walked a literal `<mod>/f*` that
    // did not exist, shipped an empty file list, and wire-deadlocked.

    #[test]
    fn resolve_sender_sources_glob_expands_module_relative_pattern() {
        // Recreate the upstream `daemon` testsuite layout: a module dir
        // with `foo/` and `bar/` subdirs. `test-from/f*` must expand to
        // `<mod>/foo` and leave `bar` alone, matching upstream's
        // glob_expand_module() behaviour.
        let tmp = tempfile::tempdir().expect("tempdir");
        let module_path = tmp.path();
        std::fs::create_dir(module_path.join("foo")).expect("foo dir");
        std::fs::create_dir(module_path.join("bar")).expect("bar dir");
        std::fs::write(module_path.join("foo").join("one"), b"one\n").expect("foo/one");

        let args = vec![".".to_owned(), "mod/f*".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "mod")
            .expect("glob pattern must resolve");
        assert_eq!(sources, vec![module_path.join("foo")]);
    }

    #[test]
    fn resolve_sender_sources_glob_keeps_literal_when_no_match() {
        // upstream: util1.c:786 - `glob.argc == save_argc` branch preserves
        // the literal arg when nothing matches so the sender surfaces a
        // normal link_stat failure (exit 23) instead of dropping silently.
        let tmp = tempfile::tempdir().expect("tempdir");
        let module_path = tmp.path();
        std::fs::create_dir(module_path.join("bar")).expect("bar dir");

        let args = vec![".".to_owned(), "mod/z*".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "mod")
            .expect("non-matching glob must resolve to literal");
        assert_eq!(sources, vec![module_path.join("z*")]);
    }

    #[test]
    fn resolve_sender_sources_glob_handles_question_mark() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let module_path = tmp.path();
        std::fs::write(module_path.join("a"), b"a").expect("a");
        std::fs::write(module_path.join("ab"), b"ab").expect("ab");
        std::fs::write(module_path.join("b"), b"b").expect("b");

        // `?` matches exactly one character; `?b` must match only `ab`.
        let args = vec![".".to_owned(), "mod/?b".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "mod")
            .expect("? glob must resolve");
        assert_eq!(sources, vec![module_path.join("ab")]);
    }

    #[test]
    fn resolve_sender_sources_glob_handles_char_class() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let module_path = tmp.path();
        std::fs::write(module_path.join("a"), b"a").expect("a");
        std::fs::write(module_path.join("b"), b"b").expect("b");
        std::fs::write(module_path.join("c"), b"c").expect("c");

        // `[ab]` matches `a` or `b` but not `c`.
        let args = vec![".".to_owned(), "mod/[ab]".to_owned()];
        let mut sources = resolve_sender_sources(module_path, &args, "mod")
            .expect("[class] glob must resolve");
        sources.sort();
        assert_eq!(sources, vec![module_path.join("a"), module_path.join("b")]);
    }

    #[test]
    fn resolve_sender_sources_glob_skips_dotfiles_by_default() {
        // POSIX glob default: a leading `.` is only matched when the pattern
        // itself starts with `.`. `*` must not match `.hidden`.
        let tmp = tempfile::tempdir().expect("tempdir");
        let module_path = tmp.path();
        std::fs::write(module_path.join(".hidden"), b"hidden").expect(".hidden");
        std::fs::write(module_path.join("visible"), b"visible").expect("visible");

        let args = vec![".".to_owned(), "mod/*".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "mod")
            .expect("* glob must resolve");
        assert_eq!(sources, vec![module_path.join("visible")]);
    }

    #[test]
    fn resolve_sender_sources_non_glob_paths_bypass_expansion() {
        // Plain paths without glob metachars must fall through unchanged,
        // even when the file does not exist on disk - upstream defers the
        // existence check to the sender's link_stat.
        let tmp = tempfile::tempdir().expect("tempdir");
        let module_path = tmp.path();

        let args = vec![".".to_owned(), "mod/missing/file".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "mod")
            .expect("plain path must resolve");
        assert_eq!(sources, vec![module_path.join("missing/file")]);
    }

    // UTS-3.b.5 - cross-platform parity for daemon sub-path resolution.
    //
    // The oc-rsync daemon never runs on Windows (preflight refuses), but a
    // Windows CLIENT can connect to a Linux daemon and trigger the same
    // `resolve_sender_sources` / `resolve_receiver_dest` helpers server-side.
    // These tests pin that the resolvers produce semantically-correct paths
    // when the module's on-disk path is in Windows drive-letter form, so a
    // future refactor that ports the daemon to Windows (or that runs these
    // helpers from a Windows host for any reason) cannot silently regress.
    //
    // The helpers join module-relative tails with a literal `/` regardless of
    // host OS (upstream `util1.c pathjoin()`), and Windows accepts mixed `/`
    // and `\` separators inside Win32 paths. The asserts below lock the exact
    // byte sequence the resolver must emit so the trailing-slash preservation
    // (upstream `flist.c:2312-2322 DOTDIR_NAME`) and the leading-separator
    // strip both survive Windows path encodings.
    //
    // UTS-3.REOPEN.c closed the Linux side via PR #5748. UTS-3.b.5 is the
    // Windows cross-platform parity attestation - no wire-format change, no
    // separator translation, just bytes-in / bytes-out coverage.

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_sender_sources_joins_with_forward_slash_on_windows_module_root() {
        // Windows daemon-mode module path with backslash separators must
        // accept module-relative positional tails and emit a joined path
        // whose suffix is exactly the literal `/<tail>` upstream's
        // pathjoin() would produce. Windows treats `C:\srv\upload/d1/d2/f2`
        // as a valid path so the sender's symlink_metadata call resolves
        // correctly without per-host separator translation.
        let module_path = std::path::Path::new(r"C:\srv\upload");
        let args = vec![".".to_owned(), "upload/d1/d2/f2".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("Windows drive-letter sub-path must resolve");
        assert_eq!(
            sources,
            vec![std::path::PathBuf::from(r"C:\srv\upload/d1/d2/f2")]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_sender_sources_preserves_trailing_slash_on_windows_module_root() {
        // Trailing-slash promotion to DOTDIR_NAME (upstream flist.c:2312-2322)
        // must survive on Windows hosts. The resolver detects the trailing
        // separator via byte-level check that already accepts both `/` and
        // `\` (client_args.rs:478), so a Windows client request like
        // `rsync://h/mod/d1/d2/` round-trips with the slash intact.
        let module_path = std::path::Path::new(r"C:\srv\upload");
        let args = vec![".".to_owned(), "upload/d1/d2/".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("Windows drive-letter sub-dir trailing-slash must resolve");
        let lossy: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(lossy, vec![r"C:\srv\upload/d1/d2/".to_owned()]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_sender_sources_accepts_backslash_terminated_module_root_on_windows() {
        // If the on-disk module path ends in `\` (e.g. an admin pasted
        // `C:\srv\upload\` into oc-rsyncd.conf), the resolver must NOT
        // double-insert a separator before the sub-path tail. The
        // needs_leading_sep check accepts trailing `\` as a valid separator
        // for Windows roots, so the joined output stays semantically equal
        // to the no-trailing-slash form rather than producing `C:\srv\upload\\d1`.
        let module_path = std::path::Path::new(r"C:\srv\upload\");
        let args = vec![".".to_owned(), "upload/d1/d2/f2".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("backslash-terminated module root must resolve");
        // The exact emitted bytes are `C:\srv\upload\` + `d1/d2/f2` because
        // the resolver detects the trailing `\` as an existing separator and
        // suppresses its own `/` insertion. The result is still a valid
        // Windows path: Win32 accepts mixed `/` and `\`.
        assert_eq!(
            sources,
            vec![std::path::PathBuf::from(r"C:\srv\upload\d1/d2/f2")]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_sender_sources_rejects_parent_dir_traversal_on_windows() {
        // Defense-in-depth must work identically on every host: a `..`
        // segment in the client positional rejects the entire request,
        // regardless of whether the module root is Linux- or Windows-style.
        let module_path = std::path::Path::new(r"C:\srv\upload");
        let args = vec![".".to_owned(), "upload/../etc/passwd".to_owned()];
        assert!(resolve_sender_sources(module_path, &args, "upload").is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_receiver_dest_joins_subpath_with_windows_module_root() {
        // Receiver-side parity: the Windows client's push destination must
        // resolve under the module root with the trailing slash preserved
        // so `--delete` and the receiver's `get_local_name` branch behave
        // the same way they do on Linux.
        let module_path = std::path::Path::new(r"C:\srv\upload");
        let args = vec![".".to_owned(), "upload/realdir/".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload").expect("valid dest");
        // Path::join uses the host separator on Windows, so a trailing
        // slash on the positional collapses into a backslash-terminated
        // PathBuf. The assertion compares via Path equality so the
        // platform's path-normalisation rules (case-insensitive drive
        // letter, separator equivalence) decide equality.
        assert_eq!(dest, std::path::PathBuf::from(r"C:\srv\upload\realdir/"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_receiver_dest_rejoins_unix_absolute_under_windows_module_root() {
        // A client-supplied positional that starts with `/` is host-absolute
        // on Linux but drive-relative on Windows. Either way the resolver
        // strips the leading separator and rejoins so the destination cannot
        // escape the module root. This pins the cross-platform SEC-1.q
        // containment guarantee on Windows hosts.
        let module_path = std::path::Path::new(r"C:\srv\upload");
        let args = vec![".".to_owned(), "/etc/passwd".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload").expect("valid dest");
        // After stripping the leading `/`, the resolver hands the bare
        // string `etc/passwd` to Path::join, which prepends the host
        // separator (`\` on Windows) but does not rewrite the embedded
        // `/`. The result is byte-identical to `C:\srv\upload\etc/passwd`,
        // which Windows still treats as a valid path because Win32 accepts
        // mixed separators.
        assert_eq!(dest, std::path::PathBuf::from(r"C:\srv\upload\etc/passwd"));
        // The destination must still live under the module root regardless
        // of separator mixing - this is the SEC-1.q containment guarantee.
        assert!(dest.starts_with(module_path));
    }

    // Ground-truth `safe_arg` reference port of upstream
    // `options.c:2539-2594` (rsync 3.4.4), option-arg branch only:
    //
    //   opt != NULL  =>  is_filename_arg = 0
    //   escapes = WILD_CHARS SHELL_CHARS
    //   WILD_CHARS  = "*?[]"
    //   SHELL_CHARS = "!#$&;|<>(){}\"'` \t\\"
    //
    // Used by the round-trip parity tests below to assert that the daemon's
    // `unbackslash_arg` reverses every character upstream's client-side
    // `safe_arg` is allowed to emit. Independent of the oc-rsync
    // `safe_arg_for_daemon` implementation so the contract is locked
    // against upstream wire output rather than against our own escape code.
    fn upstream_safe_arg_option(opt: &str, value: &str) -> String {
        const SHELL_CHARS: &[u8] = b"!#$&;|<>(){}\"'` \t\\";
        const WILD_CHARS: &[u8] = b"*?[]";
        let mut out = String::with_capacity(opt.len() + value.len() + 8);
        out.push_str(opt);
        if !opt.is_empty() {
            out.push('=');
        }
        for &byte in value.as_bytes() {
            if byte == b'\\' {
                // upstream options.c:2584-2586 - option args
                // (is_filename_arg=0) always double a literal backslash.
                out.push('\\');
            } else if WILD_CHARS.contains(&byte) || SHELL_CHARS.contains(&byte) {
                out.push('\\');
            }
            out.push(byte as char);
        }
        out
    }

    // UTS-8.REOPEN: lock the contract that the daemon's `unbackslash_arg`
    // reverses every escape upstream's `safe_arg` can emit for an option
    // arg. The asymmetric case is `--groupmap=*:GID` (upstream issue #829):
    // a 3.4.3+ non-protect_args client wraps the value with
    // `safe_arg("--groupmap", ...)`, which backslash-escapes `*` (a
    // `WILD_CHARS` member). The daemon must reverse the escape before
    // option parsing, or `--groupmap=\*:GID` reaches `parse_name_map()`
    // and the wildcard silently mismatches.
    //
    // upstream: options.c:2539-2594 safe_arg() (client-side escape)
    // upstream: io.c:1295-1306 unbackslash_arg() (daemon-side un-escape)
    #[test]
    fn unbackslash_arg_reverses_upstream_safe_arg_groupmap_wildcard() {
        let original = "--groupmap=*:42";
        let escaped = upstream_safe_arg_option("--groupmap", "*:42");
        assert_eq!(escaped, "--groupmap=\\*:42");
        assert_eq!(unbackslash_arg(&escaped), original);
    }

    // upstream: options.c:2541-2544 - `escapes = WILD_CHARS SHELL_CHARS`
    // for option args. The daemon's `unbackslash_arg` must reverse every
    // member of that set: `*?[]` (wildcards) plus `!#$&;|<>(){}\"'` \t\\`
    // (shell). A regression that drops any character from the un-escape
    // set would resurface upstream #829 for that character.
    #[test]
    fn unbackslash_arg_reverses_every_safe_arg_escape_character() {
        let escape_chars = [
            '*', '?', '[', ']', '!', '#', '$', '&', ';', '|', '<', '>', '(', ')', '{', '}', '"',
            '\'', '`', ' ', '\t', '\\',
        ];
        for &ch in &escape_chars {
            let value = format!("prefix{ch}suffix");
            let escaped = upstream_safe_arg_option("--groupmap", &value);
            // Every escape char must appear backslash-prefixed in the
            // upstream escape output so the round-trip below exercises
            // that specific char.
            assert!(
                escaped.contains(&format!("\\{ch}")),
                "upstream safe_arg should backslash-escape {ch:?}; got {escaped:?}",
            );
            let round_trip = unbackslash_arg(&escaped);
            assert_eq!(
                round_trip,
                format!("--groupmap={value}"),
                "unbackslash_arg must reverse safe_arg escape for {ch:?}",
            );
        }
    }

    // Round-trip parity for the wildcard family across `--usermap` and
    // `--groupmap` together. Mirrors upstream `options.c:2912-2916` which
    // routes both options through `safe_arg("--usermap"|"--groupmap", ...)`.
    #[test]
    fn unbackslash_arg_round_trips_usermap_groupmap_wildcards() {
        for (opt, value) in [
            ("--usermap", "*:1234"),
            ("--groupmap", "*:1234"),
            ("--usermap", "alice:bob,*:1234"),
            ("--groupmap", "wheel:0,*:1234"),
            ("--groupmap", "*:1234;dangerous"),
        ] {
            let original = format!("{opt}={value}");
            let escaped = upstream_safe_arg_option(opt, value);
            assert_eq!(
                unbackslash_arg(&escaped),
                original,
                "round-trip failed for {original:?} (escaped to {escaped:?})",
            );
        }
    }

    // upstream: authenticate.c:340-343 - an authenticated user's `:ro` suffix
    // forces read_only=1, `:rw` forces read_only=0. The per-user override must
    // win over the module's own `read only` for the session; otherwise a
    // `name:rw` user could never push to a `read only = yes` module and, worse,
    // a `name:ro` user could write to a `read only = no` module (a privilege
    // escalation). These tests pin that the override is honoured in both
    // directions and that an unsuffixed user leaves the module default intact.
    #[test]
    fn auth_ro_suffix_forces_read_only_for_session() {
        // `read only = no` module, but the user is pinned to `:ro`.
        assert!(access_effective_read_only(
            false,
            UserAccessLevel::ReadOnly
        ));
    }

    #[test]
    fn auth_rw_suffix_forces_writable_for_session() {
        // `read only = yes` module, but the user is pinned to `:rw`.
        assert!(!access_effective_read_only(
            true,
            UserAccessLevel::ReadWrite
        ));
    }

    #[test]
    fn auth_default_access_preserves_module_read_only() {
        // No suffix: the module's own `read only` setting stands unchanged.
        assert!(access_effective_read_only(true, UserAccessLevel::Default));
        assert!(!access_effective_read_only(
            false,
            UserAccessLevel::Default
        ));
    }

    #[test]
    fn auth_deny_access_preserves_module_read_only() {
        // `:deny` is refused before reaching read-only resolution, so it never
        // relaxes the module default: a denied user must not gain write access.
        assert!(access_effective_read_only(true, UserAccessLevel::Deny));
    }

    // upstream: clientserver.c:1111-1112 - `if (lp_ignore_errors(module_id))
    // ignore_errors = 1;` forces error-tolerant deletion for the session.
    #[test]
    fn module_ignore_errors_forces_config_flag() {
        let module = ModuleDefinition {
            ignore_errors: true,
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        assert!(!cfg.deletion.ignore_errors);
        apply_module_transfer_directives(&module, &mut cfg);
        assert!(cfg.deletion.ignore_errors);
    }

    #[test]
    fn module_without_ignore_errors_leaves_config_untouched() {
        let module = ModuleDefinition::default();
        let mut cfg = ServerConfig::default();
        apply_module_transfer_directives(&module, &mut cfg);
        assert!(!cfg.deletion.ignore_errors);
    }

    // upstream: clientserver.c:1201-1204 - `numeric ids = yes` forces
    // `numeric_ids = -1` for the session (NOT `1`), except under chroot when a
    // `name converter` is configured (the converter maps names inside the
    // chroot). The `-1` sentinel is load-bearing: it suppresses local name
    // resolution but keeps the uid/gid name-list on the wire, so a real
    // upstream client (whose own `numeric_ids` is `0`) still transmits the
    // list and the receiver must read it. Collapsing this into the explicit
    // `1` state (which drops the list) desyncs the receiver: it skips the
    // name-list read and misreads those bytes as the next NDX. This test pins
    // the daemon-forced state to `DaemonForced` so a future refactor cannot
    // silently reintroduce the wire desync.
    #[test]
    fn module_numeric_ids_forces_daemon_forced_state() {
        let module = ModuleDefinition {
            numeric_ids: Some(true),
            use_chroot: false,
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        assert!(cfg.flags.numeric_ids.is_off());
        apply_module_transfer_directives(&module, &mut cfg);
        // Daemon-forced, not client-explicit: keeps the wire name-list.
        assert_eq!(
            cfg.flags.numeric_ids,
            core::server::NumericIds::DaemonForced
        );
        // Local name resolution is suppressed (numeric owner preserved) ...
        assert!(cfg.flags.numeric_ids.maps_numeric());
        // ... but the wire name-list is NOT dropped (upstream `numeric_ids <= 0`).
        assert!(!cfg.flags.numeric_ids.is_explicit());
    }

    // A client that explicitly passed --numeric-ids is already in the Explicit
    // state; the daemon directive must not downgrade it and the wire list stays
    // dropped (upstream `!numeric_ids` at clientserver.c:1201 is false for `1`).
    #[test]
    fn client_explicit_numeric_ids_not_downgraded_by_module() {
        let module = ModuleDefinition {
            numeric_ids: Some(true),
            use_chroot: false,
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        cfg.flags.numeric_ids = core::server::NumericIds::Explicit;
        apply_module_transfer_directives(&module, &mut cfg);
        assert_eq!(cfg.flags.numeric_ids, core::server::NumericIds::Explicit);
        assert!(cfg.flags.numeric_ids.is_explicit());
    }

    #[test]
    fn module_numeric_ids_suppressed_by_chroot_name_converter() {
        // upstream: under chroot, a configured name converter means names can
        // still be mapped, so numeric ids is NOT forced on.
        let module = ModuleDefinition {
            numeric_ids: Some(true),
            use_chroot: true,
            name_converter: Some("/usr/bin/nc".to_owned()),
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        apply_module_transfer_directives(&module, &mut cfg);
        assert!(cfg.flags.numeric_ids.is_off());
    }

    #[test]
    fn module_numeric_ids_forced_under_chroot_without_name_converter() {
        let module = ModuleDefinition {
            numeric_ids: Some(true),
            use_chroot: true,
            name_converter: None,
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        apply_module_transfer_directives(&module, &mut cfg);
        assert_eq!(
            cfg.flags.numeric_ids,
            core::server::NumericIds::DaemonForced
        );
    }

    #[test]
    fn module_without_numeric_ids_leaves_config_untouched() {
        let module = ModuleDefinition {
            numeric_ids: Some(false),
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        apply_module_transfer_directives(&module, &mut cfg);
        assert!(cfg.flags.numeric_ids.is_off());
    }

    // upstream: clientserver.c:1201-1204 - under chroot the BOOL3 test is
    // `lp_numeric_ids(module_id) != False`, so an UNSET `numeric ids`
    // (`None`, the daemon default) forces numeric ids on. Inside the chroot
    // there is no `/etc/passwd`, so name<->id resolution is impossible and the
    // transfer must fall back to numeric ids. A default-config chrooted module
    // must therefore behave as `numeric ids = yes`, not do name-based mapping.
    #[test]
    fn module_unset_numeric_ids_forced_under_chroot() {
        let module = ModuleDefinition {
            numeric_ids: None,
            use_chroot: true,
            name_converter: None,
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        assert!(cfg.flags.numeric_ids.is_off());
        apply_module_transfer_directives(&module, &mut cfg);
        assert_eq!(
            cfg.flags.numeric_ids,
            core::server::NumericIds::DaemonForced
        );
    }

    // upstream: clientserver.c:1201-1204 - an explicit `numeric ids = no`
    // (BOOL3 `False`) is NOT overridden even under chroot, because
    // `lp_numeric_ids(module_id) != False` is false for an explicit `False`.
    #[test]
    fn module_explicit_false_numeric_ids_not_forced_under_chroot() {
        let module = ModuleDefinition {
            numeric_ids: Some(false),
            use_chroot: true,
            name_converter: None,
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        apply_module_transfer_directives(&module, &mut cfg);
        assert!(cfg.flags.numeric_ids.is_off());
    }

    // upstream: clientserver.c:1201-1204 - without chroot the BOOL3 test is
    // `lp_numeric_ids(module_id) == True`, so an UNSET `numeric ids` stays at
    // the client's default and is NOT forced on.
    #[test]
    fn module_unset_numeric_ids_not_forced_without_chroot() {
        let module = ModuleDefinition {
            numeric_ids: None,
            use_chroot: false,
            name_converter: None,
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        apply_module_transfer_directives(&module, &mut cfg);
        assert!(cfg.flags.numeric_ids.is_off());
    }
}
