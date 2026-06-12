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

        let err = verify_secret_response(&module, "alice", "challenge", "response", None)
            .expect_err("strict modes should reject other-accessible secrets");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
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
        let result = verify_secret_response(&module, "alice", "challenge", "response", None)
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
        let result = verify_secret_response(&module, "alice", "challenge", "response", None)
            .expect("should not error on permissions");
        assert!(!result, "auth should fail due to wrong response");
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
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest, std::path::Path::new("/srv/upload/realdir/"));
    }

    #[test]
    fn resolve_receiver_dest_falls_back_to_module_root_for_bare_module() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest, std::path::Path::new("/srv/upload"));
    }

    #[test]
    fn resolve_receiver_dest_falls_back_to_module_root_when_no_positional() {
        let module_path = std::path::Path::new("/srv/upload");
        let args: Vec<String> = vec![];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
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
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest, std::path::Path::new("/srv/upload/destdir/"));
    }

    #[test]
    fn resolve_receiver_dest_rejoins_absolute_path_under_module_root() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "/etc/passwd".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        // Absolute path is forced under the module root - no escape.
        assert_eq!(dest, std::path::Path::new("/srv/upload/etc/passwd"));
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

    #[test]
    fn resolve_sender_sources_returns_module_root_without_positional() {
        // upstream: clientserver.c:1073 - bare module request (no sub-path)
        // means the sender walks the module root directly. Pre-fix behaviour
        // must be preserved exactly to avoid regressing the existing pull
        // tests that target `rsync://h/module/`.
        let module_path = std::path::Path::new("/srv/upload");
        let args: Vec<String> = vec![];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("bare module request must resolve");
        assert_eq!(sources, vec![std::path::PathBuf::from("/srv/upload")]);
    }

    #[test]
    fn resolve_sender_sources_returns_module_root_for_empty_subpath() {
        // upstream: util1.c:813-814 - `module/` strips to "" after
        // glob_expand_module; the daemon sender should still walk the module
        // root and emit "." with FLAG_TOP_DIR.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload")
            .expect("empty sub-path must resolve");
        assert_eq!(sources, vec![std::path::PathBuf::from("/srv/upload")]);
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
}
