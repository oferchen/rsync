#[cfg(test)]
mod module_access_tests {
    use super::*;
    use std::io::Cursor;

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

    /// upstream: log.c:163 - log-open failures produce RERR_MESSAGEIO (13).
    #[test]
    fn log_file_error_creates_daemon_error_with_correct_code() {
        let path = std::path::Path::new("/tmp/test.log");
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "test error");
        let err = log_file_error(path, io_err);
        assert_eq!(
            err.exit_code(),
            core::exit_code::ExitCode::MessageIo.as_i32()
        );
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

    /// A resolved name is rendered verbatim, whatever the lookup setting.
    #[test]
    fn peer_host_display_returns_the_resolved_name() {
        assert_eq!(peer_host_display(Some("example.com"), true), "example.com");
        assert_eq!(peer_host_display(Some("example.com"), false), "example.com");
    }

    /// A lookup that was ATTEMPTED and produced nothing renders `UNKNOWN`.
    ///
    /// upstream: clientname.c:112 `strlcpy(name_buf, default_name, ...)`.
    #[test]
    fn peer_host_display_reports_unknown_when_the_lookup_failed() {
        assert_eq!(peer_host_display(None, true), "UNKNOWN");
    }

    /// A lookup that was never attempted renders `UNDETERMINED`, a DIFFERENT
    /// string. Collapsing the two is the divergence this replaces: oc printed
    /// one lowercase `unknown` for both, and the bare IP in the access lines.
    ///
    /// upstream: clientserver.c:126 + :1525.
    #[test]
    fn peer_host_display_reports_undetermined_when_no_lookup_ran() {
        assert_eq!(peer_host_display(None, false), "UNDETERMINED");
        assert_ne!(
            peer_host_display(None, false),
            peer_host_display(None, true)
        );
    }

    /// The sentinels must never be an address: the daemon log renders
    /// `<host> (<addr>)` and upstream keeps those two fields distinct even when
    /// the host is unknown. oc previously printed `10.0.0.1 (10.0.0.1)`.
    #[test]
    fn peer_host_display_never_substitutes_the_peer_address() {
        for reverse_lookup in [true, false] {
            let rendered = peer_host_display(None, reverse_lookup);
            assert!(rendered.parse::<std::net::IpAddr>().is_err(), "{rendered}");
        }
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

    // upstream: clientserver.c:1254 governs the format_daemon_module_listing
    // wire layout exercised below.

    #[test]
    fn module_listing_format_short_name_padded_to_15() {
        // upstream: %-15s pads short names with trailing spaces
        let line = format_daemon_module_listing("docs", "Documentation");
        assert_eq!(line, "docs           \tDocumentation\n");
    }

    #[test]
    fn module_listing_format_exact_15_char_name() {
        // A name exactly 15 characters wide should have no extra padding
        let line = format_daemon_module_listing("exactly15chars_", "comment");
        assert_eq!(line, "exactly15chars_\tcomment\n");
    }

    #[test]
    fn module_listing_format_name_longer_than_15() {
        // upstream: %-15s does not truncate - names wider than 15 chars extend the field
        let line = format_daemon_module_listing("very_long_module_name", "A long name module");
        assert_eq!(line, "very_long_module_name\tA long name module\n");
    }

    #[test]
    fn module_listing_format_empty_comment() {
        // upstream: lp_comment(i) returns "" for modules without a comment directive
        let line = format_daemon_module_listing("backup", "");
        assert_eq!(line, "backup         \t\n");
    }

    #[test]
    fn module_listing_format_single_char_name() {
        let line = format_daemon_module_listing("x", "tiny");
        assert_eq!(line, "x              \ttiny\n");
    }

    #[test]
    fn module_listing_format_empty_name() {
        // Edge case: empty module name still gets padded to 15 spaces
        let line = format_daemon_module_listing("", "orphan");
        assert_eq!(line, "               \torphan\n");
    }

    #[test]
    fn module_listing_format_tab_separator_present() {
        // The separator between name field and comment must be exactly one tab
        let line = format_daemon_module_listing("test", "hello");
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
        let line = format_daemon_module_listing("mod", "comment");
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
        let result = verify_secret_response(
            &module,
            "alice",
            None,
            "challenge",
            "response",
            DaemonAuthDigest::Md5,
        )
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
        let result = verify_secret_response(
            &module,
            "alice",
            None,
            "challenge",
            "response",
            DaemonAuthDigest::Md5,
        )
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
        let result = verify_secret_response(
            &module,
            "alice",
            None,
            "challenge",
            "response",
            DaemonAuthDigest::Md5,
        )
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
        let granted = verify_secret_response(
            &module,
            "alice",
            Some("devs"),
            challenge,
            &response,
            DaemonAuthDigest::Md5,
        )
        .expect("no io error");
        assert!(
            granted,
            "group member must authenticate via @devs shared secret"
        );

        // upstream: authenticate.c:318 - a plain-username authorization passes a
        // NULL group, so `@group:` lines are never consulted. Denied here.
        let denied = verify_secret_response(
            &module,
            "alice",
            None,
            challenge,
            &response,
            DaemonAuthDigest::Md5,
        )
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
        let denied = verify_secret_response(
            &module,
            "alice",
            None,
            challenge,
            &response,
            DaemonAuthDigest::Md5,
        )
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
        let granted = verify_secret_response(
            &module_ok,
            "alice",
            None,
            challenge,
            &response,
            DaemonAuthDigest::Md5,
        )
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
        let full_args = protocol::secluded_args::recv_secluded_args(&mut reader, None, None)
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

    /// upstream: options.c:3014-3015 / generator.c:2481 - a daemon receiver
    /// invoked with `--force` must set the second term of `delete_mode ||
    /// force_delete`, which is what lets a POPULATED directory obstacle be
    /// cleared. This is the SECOND server-arg parser: the stdio `--server` path
    /// never reaches it, so wiring only that one leaves daemon uploads refusing
    /// at 23.
    #[test]
    fn apply_long_form_args_parses_force() {
        let mut without = ServerConfig::default();
        let _ = apply_long_form_args(&["--server".to_owned(), ".".to_owned()], &mut without);
        assert!(!without.flags.force, "default must be false");

        let mut with = ServerConfig::default();
        let _ = apply_long_form_args(
            &["--server".to_owned(), "--force".to_owned(), ".".to_owned()],
            &mut with,
        );
        assert!(with.flags.force, "--force must set the flag");
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
        let args = vec![
            "--server".to_owned(),
            "--log-format=%i".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_parses_log_format_with_itemize_and_upper_i() {
        let args = vec![
            "--server".to_owned(),
            "--log-format=%i%I".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_parses_out_format_with_itemize() {
        let args = vec![
            "--server".to_owned(),
            "--out-format=%i".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_log_format_without_itemize() {
        let args = vec![
            "--server".to_owned(),
            "--log-format=%o".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        let _ = apply_long_form_args(&args, &mut config);
        assert!(!config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_long_form_args_log_format_x_no_itemize() {
        let args = vec![
            "--server".to_owned(),
            "--log-format=X".to_owned(),
            ".".to_owned(),
        ];
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
        assert_eq!(
            offender,
            Some(ClientArgRejection::Unrecognized(
                "--write-batch=/tmp/bad.batch".to_owned()
            ))
        );
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
        assert_eq!(
            offender,
            Some(ClientArgRejection::Unrecognized(
                "--read-batch=/tmp/in.batch".to_owned()
            ))
        );
    }

    // `--only-write-batch` is the one member of the batch family upstream
    // deliberately forwards to the server: `options.c:3016-3017` emits the
    // literal `--only-write-batch=X` inside `server_options()`'s `am_sender`
    // block, and `options.c:812` accepts it into the same popt table the daemon
    // runs. Refusing it here turned away a conforming upstream 3.5.0 client with
    // `@ERROR: --only-write-batch=X: unrecognized option (in daemon mode)` and
    // exit 4, where upstream's own daemon completes the push writing nothing.
    //
    // The daemon must take the mode switch from it, not the value: the `X` is a
    // placeholder (`main.c:1912` gates `open_batch_files()` on `!am_server`),
    // while `clientserver.c:1195` sets `dry_run = 1` and `receiver.c:987-993`
    // logs each item without touching the destination.
    #[test]
    fn apply_long_form_args_accepts_only_write_batch_and_forces_dry_run() {
        let args = vec![
            "--server".to_owned(),
            "--only-write-batch=X".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig::default();
        assert_eq!(config.role, ServerRole::Receiver);
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(
            config.flags.only_write_batch,
            "--only-write-batch=X must select the receiver's only-write-batch loop"
        );
        assert!(
            config.flags.dry_run,
            "upstream clientserver.c:1195 forces dry_run when write_batch < 0"
        );
    }

    // `server_options()` emits the token only inside its `am_sender` block, so a
    // daemon serving a PULL is the sender and never sees it. A generator that
    // switched itself into dry-run would stop streaming the data the client
    // needs to record, so the arm must stay inert for that role.
    #[test]
    fn apply_long_form_args_leaves_a_sender_daemon_untouched_by_only_write_batch() {
        let args = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "--only-write-batch=X".to_owned(),
            ".".to_owned(),
        ];
        let mut config = ServerConfig {
            role: ServerRole::Generator,
            ..ServerConfig::default()
        };
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(!config.flags.only_write_batch);
        assert!(!config.flags.dry_run);
    }

    // upstream: options.c:2998-3001 - `server_options()` forwards
    // `--min-size`/`--max-size` only under `if (am_sender)`, i.e. only to a
    // daemon that is RECEIVING a push, because enforcement lives in the
    // generator (generator.c:2118-2133) on the receiving side. Dropping the
    // value made a push deposit exactly the files the client asked to
    // exclude - measured against real rsync 3.5.0, which lands only the small
    // file where oc landed both.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_max_size() {
        let args = vec![
            "--server".to_owned(),
            "-logDtpr".to_owned(),
            "--max-size=100".to_owned(),
            ".".to_owned(),
            "module/".to_owned(),
        ];
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(config.file_selection.max_file_size, Some(100));
    }

    #[test]
    fn apply_long_form_args_honours_a_forwarded_min_size_with_a_suffix() {
        let args = vec![
            "--server".to_owned(),
            "-logDtpr".to_owned(),
            "--min-size=1K".to_owned(),
            ".".to_owned(),
            "module/".to_owned(),
        ];
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(config.file_selection.min_file_size, Some(1024));
    }

    /// Builds the client argv a daemon sees: the compact flag string, then the
    /// long-form options, then the `.` separator upstream's `server_options()`
    /// emits before the module-relative paths.
    fn daemon_argv(flag_string: &str, long_form: &[&str]) -> Vec<String> {
        let mut args = vec!["--server".to_owned(), flag_string.to_owned()];
        args.extend(long_form.iter().map(|a| (*a).to_owned()));
        args.push(".".to_owned());
        args.push("module/".to_owned());
        args
    }

    // Every case below is an option upstream's `server_options()` emits to a
    // daemon and that oc's own `--server` argv parser already honours, but that
    // the daemon's long-form parser dropped. Dropping is silent
    // (`is_client_only_flag_reaching_daemon` only ever fires for the batch
    // family), so each of these was a live behaviour difference between the two
    // transports rather than a diagnostic.

    // upstream: options.c:2914 - `if (list_only > 1) args[ac++] = "--list-only"`.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_list_only() {
        let args = daemon_argv("-logDtpr", &["--list-only"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(config.flags.list_only);
    }

    // The negations below run against a config whose field is already `true`,
    // because that is the state the compact flag string leaves behind:
    // `ServerConfig::from_flag_string_and_args` parses the letters first and
    // `apply_long_form_args` runs on its result. Starting from the struct
    // default would let a missing arm pass, since the default is already the
    // value being asserted.

    // upstream: options.c:2917-2919 - `-d --delete` on the client emits `--no-r`
    // so the remote may delete without `-r`; options.c:632 clears the same
    // `recurse` global the compact `r` letter set.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_no_r() {
        let args = daemon_argv("-logDtpr", &["--no-r"]);
        let mut config = ServerConfig::default();
        config.flags.recursive = true;
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(!config.flags.recursive);
    }

    // upstream: options.c:2926-2930 - `-D` covers devices only, so a client that
    // preserves devices but not specials sends `--no-specials`.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_no_specials() {
        let args = daemon_argv("-logDtpr", &["--no-specials"]);
        let mut config = ServerConfig::default();
        config.flags.specials = true;
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(!config.flags.specials);
    }

    // upstream: options.c:2931-2932 - `--specials` without `-D` is the other
    // half of the same branch: specials preserved, devices not.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_specials() {
        let args = daemon_argv("-logtpr", &["--specials"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(config.flags.specials);
    }

    // upstream: options.c:2948-2951 - the two spellings set the same global to 1
    // and 0. Feeding both in order proves the negation arm exists and wins,
    // which asserting the default alone could not.
    #[test]
    fn apply_long_form_args_honours_both_msgs2stderr_spellings() {
        let mut config = ServerConfig::default();
        let args = daemon_argv("-logDtpr", &["--msgs2stderr"]);
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(config.flags.msgs_to_stderr);

        let args = daemon_argv("-logDtpr", &["--msgs2stderr", "--no-msgs2stderr"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(!config.flags.msgs_to_stderr);
    }

    // upstream: options.c:3059-3060 - `else if (keep_partial && am_sender)`
    // sends the bare `--partial` to a daemon receiver.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_partial() {
        let args = daemon_argv("-logDtpr", &["--partial"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(config.flags.partial);
    }

    // upstream: options.c:3128-3130 - an `--inplace --sparse` sender emits
    // `--no-W` so the receiver still asks for a delta; options.c:760 clears the
    // same `whole_file` global the compact `W` letter set.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_no_whole_file() {
        let args = daemon_argv("-logDtprW", &["--inplace", "--no-W"]);
        let mut config = ServerConfig::default();
        config.flags.whole_file = true;
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(!config.flags.whole_file);
    }

    // upstream: options.c:3143-3144 - a `--files-from` transfer with relative
    // paths off sends `--no-relative`; options.c:707-708 spell it both ways.
    #[test]
    fn apply_long_form_args_honours_both_no_relative_spellings() {
        for spelling in ["--no-relative", "--no-R"] {
            let args = daemon_argv("-logDtprR", &[spelling]);
            let mut config = ServerConfig::default();
            config.flags.relative = true;
            assert!(apply_long_form_args(&args, &mut config).is_none());
            assert!(!config.flags.relative, "{spelling} must clear relative");
        }
    }

    // upstream: options.c:3153-3156 - the daemon SENDER unlinks each source file
    // once the receiver acknowledges it. Dropping this left every source in
    // place on a daemon pull that asked for them to be moved.
    #[test]
    fn apply_long_form_args_honours_both_remove_source_files_spellings() {
        for spelling in ["--remove-source-files", "--remove-sent-files"] {
            let args = daemon_argv("-logDtpr", &[spelling]);
            let mut config = ServerConfig::default();
            assert!(apply_long_form_args(&args, &mut config).is_none());
            assert!(
                config.flags.remove_source_files,
                "{spelling} must request source removal"
            );
        }
    }

    // upstream: options.c:3161-3162 - forwarded to a daemon receiver so it
    // fallocate()s each destination file before writing.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_preallocate() {
        let args = daemon_argv("-logDtpr", &["--preallocate"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(config.flags.preallocate);
    }

    // upstream: options.c:3164-3165 - forwarded so the daemon sender opens
    // source files O_NOATIME.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_open_noatime() {
        let args = daemon_argv("-logDtpr", &["--open-noatime"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert!(config.write.open_noatime);
    }

    // upstream: compat.c:544 - the checksum vstring is sent only when
    // `!checksum_choice`. A daemon that dropped the option kept sending a list
    // its peer had already decided not to read, which desyncs the exchange;
    // this is the wire-affecting member of the set, not a preference.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_checksum_choice() {
        let args = daemon_argv("-logDtpr", &["--checksum-choice=xxh3"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(
            config.checksum_choice,
            Some(::protocol::ChecksumAlgorithm::XXH3)
        );
    }

    // upstream: checksum.c:196-202 - a comma splits the spec into the transfer
    // sum and the whole-file sum; only the transfer half reaches the wire
    // negotiation, so taking the whole string would reject a legal spec.
    #[test]
    fn apply_long_form_args_takes_the_transfer_half_of_a_checksum_choice_pair() {
        let args = daemon_argv("-logDtpr", &["--checksum-choice=xxh3,md5"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(
            config.checksum_choice,
            Some(::protocol::ChecksumAlgorithm::XXH3)
        );
    }

    // upstream: checksum.c:127-140 parse_csum_name() - `auto` is a legal name
    // that resolves to md5, not an error. oc's client resolves `auto,<file>` to
    // MD5 before forwarding it, so the daemon must land on the same algorithm
    // or the two ends disagree about a checksum neither of them negotiated.
    #[test]
    fn apply_long_form_args_resolves_an_auto_checksum_choice_to_md5() {
        let args = daemon_argv("-logDtpr", &["--checksum-choice=auto,md5"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(
            config.checksum_choice,
            Some(::protocol::ChecksumAlgorithm::MD5)
        );
    }

    // upstream: checksum.c:156 - an unknown name is `RERR_UNSUPPORTED`, not a
    // silent fallback. Falling back would pick an algorithm the peer is not
    // using, which corrupts the comparison rather than failing it.
    #[test]
    fn apply_long_form_args_rejects_an_unknown_checksum_choice() {
        let args = daemon_argv("-logDtpr", &["--checksum-choice=nope"]);
        let mut config = ServerConfig::default();
        assert_eq!(
            apply_long_form_args(&args, &mut config),
            Some(ClientArgRejection::InvalidValue(
                "unknown checksum name: nope".to_owned()
            ))
        );
    }

    // upstream: options.c:3046-3049 + compat.c:823-825 - the SERVER picks the
    // seed and writes it on the wire, so a daemon that drops the client's value
    // makes `--checksum-seed` a no-op over a daemon while it works over ssh.
    #[test]
    fn apply_long_form_args_honours_a_forwarded_checksum_seed() {
        let args = daemon_argv("-logDtpr", &["--checksum-seed=12345"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(config.checksum_seed, Some(12345));
    }

    // upstream: options.c:861 is POPT_ARG_INT and options.c:3047 prints `%d`, so
    // the value can be negative. `write_seed` puts it back on the wire as the
    // same 32 bits, so a u32-only parse would drop exactly the values upstream
    // can emit.
    #[test]
    fn apply_long_form_args_accepts_a_negative_checksum_seed() {
        let args = daemon_argv("-logDtpr", &["--checksum-seed=-5"]);
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(config.checksum_seed, Some(-5));
    }

    // upstream: options.c:1172-1175 - the digit scan leaves the cursor on the
    // terminator, so an empty value parses as 0. For `--max-size` that means
    // "exclude every non-empty file", NOT "no limit"; treating it as absent
    // would ship the files upstream withholds.
    #[test]
    fn apply_long_form_args_treats_an_empty_max_size_as_zero() {
        let args = vec!["--server".to_owned(), "--max-size=".to_owned()];
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(config.file_selection.max_file_size, Some(0));
    }

    // upstream: options.c:1808-1817 - a `parse_size_arg` failure aborts option
    // parsing, and options.c:1253 renders `--%s=%s is %s`. Silently ignoring
    // the value would re-open the drop this arm exists to close, so the
    // rejection carries the value's own message rather than the
    // unknown-option one.
    #[test]
    fn apply_long_form_args_rejects_an_unparseable_max_size() {
        let args = vec!["--server".to_owned(), "--max-size=12Q".to_owned()];
        let mut config = ServerConfig::default();
        assert_eq!(
            apply_long_form_args(&args, &mut config),
            Some(ClientArgRejection::InvalidValue(
                "--max-size=12Q is invalid".to_owned()
            ))
        );
        assert_eq!(config.file_selection.max_file_size, None);
    }

    // Boundary MEASURED against real rsync 3.5.0, not derived from the C by
    // reading: upstream's range check compares the strtod `double` with a
    // strict `dsize >= size_max` (options.c:1216-1221), and
    // `(double)(SIZE_MAX / 2)` rounds to 2^63 - so `i64::MAX` itself is
    // already "too large" and the largest accepted value is 2^63 - 1024.
    // An integer comparison would accept that whole band.
    #[test]
    fn apply_long_form_args_rejects_a_min_size_above_upstreams_ceiling() {
        for value in ["9223372036854775807", "9223372036854775806", "8192P"] {
            let args = vec!["--server".to_owned(), format!("--min-size={value}")];
            let mut config = ServerConfig::default();
            assert_eq!(
                apply_long_form_args(&args, &mut config),
                Some(ClientArgRejection::InvalidValue(format!(
                    "--min-size={value} is too large"
                ))),
                "{value} must be refused exactly as upstream refuses it"
            );
            assert_eq!(config.file_selection.min_file_size, None, "{value}");
        }
    }

    // Non-vacuity companion: the greatest value upstream ACCEPTS must still be
    // accepted, so the ceiling cannot pass by rejecting everything large.
    #[test]
    fn apply_long_form_args_accepts_the_largest_value_upstream_allows() {
        for (value, bytes) in [
            ("9223372036854774784", 9_223_372_036_854_774_784u64),
            ("8191P", 8191 * 1024 * 1024 * 1024 * 1024 * 1024),
        ] {
            let args = vec!["--server".to_owned(), format!("--min-size={value}")];
            let mut config = ServerConfig::default();
            assert!(
                apply_long_form_args(&args, &mut config).is_none(),
                "{value} must be accepted exactly as upstream accepts it"
            );
            assert_eq!(config.file_selection.min_file_size, Some(bytes), "{value}");
        }
    }

    // A positional operand that happens to look like the option must not be
    // decoded: everything after the standalone `.` is a path, and upstream
    // consumes it through `glob_expand_module()` instead.
    #[test]
    fn apply_long_form_args_ignores_a_max_size_shaped_operand() {
        let args = vec![
            "--server".to_owned(),
            ".".to_owned(),
            "--max-size=12Q".to_owned(),
        ];
        let mut config = ServerConfig::default();
        assert!(apply_long_form_args(&args, &mut config).is_none());
        assert_eq!(config.file_selection.max_file_size, None);
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
        assert!(
            offender.is_none(),
            "no unknown should be reported: {offender:?}"
        );
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
        assert!(
            offender.is_none(),
            "positional paths must not flag: {offender:?}"
        );
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

    #[test]
    fn reopen_module_log_sink_targets_each_modules_log_file() {
        // upstream: log.c:169-204 `log_init(1)` at clientserver.c:897 - selecting
        // a module reopens the daemon log to that module's `log file`, so two
        // modules with distinct `log file` directives write their diagnostics to
        // distinct files rather than sharing one sink.
        let dir = tempfile::tempdir().expect("temp dir");
        let path_a = dir.path().join("a.log");
        let path_b = dir.path().join("b.log");

        let module_a = ModuleDefinition {
            log_file: Some(path_a.clone()),
            ..Default::default()
        };
        let module_b = ModuleDefinition {
            log_file: Some(path_b.clone()),
            ..Default::default()
        };

        let sink_a = reopen_module_log_sink(&module_a, None).expect("module a sink");
        let sink_b = reopen_module_log_sink(&module_b, None).expect("module b sink");
        log_message(&sink_a, &rsync_info!("entry-for-module-a"));
        log_message(&sink_b, &rsync_info!("entry-for-module-b"));
        drop(sink_a);
        drop(sink_b);

        let a = std::fs::read_to_string(&path_a).expect("read a.log");
        let b = std::fs::read_to_string(&path_b).expect("read b.log");
        assert!(a.contains("entry-for-module-a"), "a.log: {a}");
        assert!(!a.contains("entry-for-module-b"), "a.log leaked b: {a}");
        assert!(b.contains("entry-for-module-b"), "b.log: {b}");
        assert!(!b.contains("entry-for-module-a"), "b.log leaked a: {b}");
    }

    #[test]
    fn reopen_module_log_sink_none_without_log_file() {
        // A module with no `log file` leaves the startup sink in place: upstream's
        // `lp_log_file(module_id)` is empty, so `log_init(1)` keeps the current
        // log rather than reopening.
        let module = ModuleDefinition::default();
        assert!(reopen_module_log_sink(&module, None).is_none());
    }

    #[test]
    fn builtin_dont_compress_default_does_not_collapse_stream() {
        // upstream: token.c:206-211 - only a bare `*` in `dont compress`
        // collapses the whole compression stream to store level; the per-suffix
        // lookup in set_compression is compiled out (`#if 0`, token.c:227). The
        // built-in DEFAULT_DONT_COMPRESS list therefore never matches-all, so a
        // module that inherits it still compresses a `.gz` exactly as upstream
        // 3.4.4 does. Seeding the default is config fidelity, not a wire change.
        assert!(!dont_compress_is_match_all(DEFAULT_DONT_COMPRESS));
    }

    fn test_module_with_defaults() -> ModuleRuntime {
        ModuleRuntime::from(ModuleDefinition::default())
    }

    /// `insecure links = yes` must actually reach the ownership walk's opt-out.
    ///
    /// upstream: `syscall.c:117-126` - for a daemon, `symlink_optout_allowed()`
    /// IS `module_id >= 0 && lp_insecure_links(module_id)`. Without this pin the
    /// directive could parse and be stored while the walk stayed fully engaged:
    /// an inert change that every parse-only test would still call green.
    ///
    /// Safe against the process-global `SESSION_OPTOUT` because this workspace
    /// runs under cargo-nextest, which executes each test in its own process.
    #[test]
    fn insecure_links_directive_reaches_the_session_optout() {
        let module = ModuleRuntime::from(ModuleDefinition {
            insecure_links: true,
            ..Default::default()
        });
        publish_module_confinement(&module, std::path::Path::new("/srv/module"), false);
        assert!(
            fast_io::confinement::session_optout_allowed(),
            "`insecure links = yes` must disengage the operator-path walk"
        );
    }

    /// Non-vacuity companion for the pin above: with the directive absent the
    /// confinement stays engaged, so that test measures the directive and not a
    /// predicate that is unconditionally true.
    /// The SAME directive must also release the Landlock sandbox. Without this
    /// the ownership walk opts out while the kernel allowlist - pinned to
    /// `module.path` - still refuses the read, so `insecure links = yes` is
    /// accepted, logged, and silently inoperative. Measured on CI before the
    /// fix: both modules sent identical byte counts and the daemon logged
    /// "landlock fully enforced over 1 root(s)" for the opted-out module.
    #[test]
    fn insecure_links_also_releases_the_landlock_sandbox() {
        let module = ModuleRuntime::from(ModuleDefinition {
            insecure_links: true,
            ..Default::default()
        });
        assert!(
            landlock_skip_reason(&module).is_some(),
            "`insecure links = yes` must skip Landlock; leaving it engaged makes the directive inoperative"
        );
    }

    /// Non-vacuity companion: a default module must still be sandboxed. Without
    /// this the test above also passes if the skip were made unconditional.
    #[test]
    fn a_default_module_is_still_landlock_sandboxed() {
        let module = test_module_with_defaults();
        assert_eq!(
            landlock_skip_reason(&module),
            None,
            "a module with no opt-out and no exec hook must keep Landlock engaged"
        );
    }

    /// The pre-existing arm must survive the extraction - it is a separate
    /// operator decision with a different reason, not a duplicate of the above.
    #[test]
    fn a_configured_exec_hook_still_skips_landlock() {
        let module = ModuleRuntime::from(ModuleDefinition {
            pre_xfer_exec: Some("/usr/local/bin/notify.sh".into()),
            ..Default::default()
        });
        let reason = landlock_skip_reason(&module).expect("exec hook must skip landlock");
        assert!(
            reason.contains("xfer-exec"),
            "the hook arm must report its own reason, got: {reason}"
        );
    }

    /// BOTH kernel layers must stand aside for the same module, from the same
    /// predicate. They used to disagree: Landlock skipped for the operator's
    /// declared hook while the seccomp filter stayed engaged and EPERMed the
    /// `execve` the hook needs, so the hook silently never ran.
    #[test]
    fn an_exec_hook_releases_both_kernel_sandbox_layers() {
        for module in [
            ModuleRuntime::from(ModuleDefinition {
                pre_xfer_exec: Some("/usr/local/bin/before.sh".into()),
                ..Default::default()
            }),
            ModuleRuntime::from(ModuleDefinition {
                post_xfer_exec: Some("/usr/local/bin/after.sh".into()),
                ..Default::default()
            }),
        ] {
            let hook =
                exec_hook_skip_reason(&module).expect("an exec hook must release the layers");
            assert_eq!(
                landlock_skip_reason(&module),
                Some(hook),
                "landlock must skip for the same reason the shared predicate gives"
            );
        }
    }

    /// Non-vacuity companion: without a hook neither layer stands aside, so the
    /// test above cannot pass by making the skip unconditional.
    #[test]
    fn a_module_without_an_exec_hook_keeps_both_layers() {
        let module = test_module_with_defaults();
        assert_eq!(exec_hook_skip_reason(&module), None);
        assert_eq!(landlock_skip_reason(&module), None);
    }

    /// `insecure links` releases Landlock ONLY - it is a path-confinement
    /// decision and says nothing about the syscall filter. Pinning the
    /// asymmetry keeps a future "make both layers agree" edit from widening it.
    #[test]
    fn insecure_links_does_not_release_the_seccomp_layer() {
        let module = ModuleRuntime::from(ModuleDefinition {
            insecure_links: true,
            ..Default::default()
        });
        assert!(landlock_skip_reason(&module).is_some());
        assert_eq!(
            exec_hook_skip_reason(&module),
            None,
            "`insecure links` is about paths, not syscalls; seccomp must stay engaged"
        );
    }

    #[test]
    fn absent_insecure_links_leaves_the_confinement_engaged() {
        let module = test_module_with_defaults();
        publish_module_confinement(&module, std::path::Path::new("/srv/module"), false);
        assert!(
            !fast_io::confinement::session_optout_allowed(),
            "a module without the directive must keep the walk engaged"
        );
    }

    /// A module served WITHOUT chroot keeps the real module path as its
    /// Landlock root - the arm that always worked, pinned so the chroot arms
    /// below cannot be satisfied by making the new root selection
    /// unconditional.
    #[test]
    fn an_unchrooted_module_pins_landlock_to_the_real_module_path() {
        let module = ModuleRuntime::from(ModuleDefinition {
            path: PathBuf::from("/srv/data"),
            ..Default::default()
        });
        assert_eq!(
            landlock_root(&module, &PrivilegeOutcome::not_chrooted()),
            LandlockRoot::Confine(Path::new("/srv/data")),
        );
    }

    /// WHY: `engage_landlock_sandbox` runs AFTER `chroot()`, and
    /// `restrict_to_module_paths` OPENS every root it is given. Handing it the
    /// pre-chroot `module.path` therefore cannot work: the kernel returns
    /// ENOENT and the layer degrades to a warning. MEASURED on Linux 7.0
    /// x86_64 before this pin, with `use chroot = yes` and `path = /srv/data`:
    /// `landlock setup failed: failed to open "/srv/data": No such file or
    /// directory`, the transfer completing with chroot as the only
    /// confinement. So: never the pre-chroot path once chrooted.
    #[test]
    fn a_chrooted_module_never_pins_landlock_to_the_pre_chroot_path() {
        let module = ModuleRuntime::from(ModuleDefinition {
            path: PathBuf::from("/srv/data"),
            ..Default::default()
        });
        for inner in [
            Some(PathBuf::from("/")),
            Some(PathBuf::from("/inner")),
            None,
        ] {
            let outcome = PrivilegeOutcome {
                chroot_applied: true,
                inner_module_path: inner.clone(),
            };
            assert_ne!(
                landlock_root(&module, &outcome),
                LandlockRoot::Confine(Path::new("/srv/data")),
                "post-chroot the pre-chroot path opens nothing; inner={inner:?}"
            );
        }
    }

    /// A chroot whose root IS the module root subsumes the allowlist, so the
    /// layer must stand aside as a TAKEN branch rather than attempt an install
    /// that cannot succeed. Pinning `SubsumedByChroot` (not merely "not the old
    /// path") keeps a future edit from substituting the post-chroot `/`: the
    /// `READONLY_SYSTEM_PATHS` rules resolve inside the jail and a deeper
    /// Landlock rule overrides a shallower one, so a module tree containing
    /// `etc/` or `var/` would silently become read-only.
    ///
    /// upstream: clientserver.c:912-925 sets `module_dir = "/"` then zeroes
    /// `module_dirlen`, so clientserver.c:1093
    /// `use_secure_symlinks = am_daemon && (!am_chrooted || module_dirlen)`
    /// is false here - upstream reaches the same conclusion for its own
    /// path-confinement layer.
    #[test]
    fn a_chroot_at_the_module_root_subsumes_landlock() {
        let module = ModuleRuntime::from(ModuleDefinition {
            path: PathBuf::from("/srv/data"),
            ..Default::default()
        });
        let outcome = PrivilegeOutcome {
            chroot_applied: true,
            inner_module_path: Some(PathBuf::from("/")),
        };
        assert_eq!(
            landlock_root(&module, &outcome),
            LandlockRoot::SubsumedByChroot,
        );
    }

    /// The `/./` shape is the one that genuinely needs Landlock: the chroot
    /// lands on the OUTER path, so nothing kernel-side confines the inner
    /// module boundary. Pin the post-chroot inner directory as the root.
    ///
    /// upstream: clientserver.c:1084-1093 - "the kernel chroot confines the
    /// outer path but not the inner module", which is why upstream keeps its
    /// own secure-symlink path active for exactly this configuration.
    #[test]
    fn a_chroot_with_an_inner_boundary_pins_landlock_to_the_inner_path() {
        let module = ModuleRuntime::from(ModuleDefinition {
            path: PathBuf::from("/srv/outer/./inner"),
            ..Default::default()
        });
        let outcome = PrivilegeOutcome {
            chroot_applied: true,
            inner_module_path: Some(PathBuf::from("/inner")),
        };
        assert_eq!(
            landlock_root(&module, &outcome),
            LandlockRoot::Confine(Path::new("/inner")),
        );
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
        assert!(rule.pattern.to_string_lossy().ends_with("/***"));
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
        assert_eq!(
            strip_keyword_prefix("exclude *.tmp", "exclude"),
            Some("*.tmp")
        );
    }

    #[test]
    fn strip_keyword_prefix_comma_separator() {
        assert_eq!(
            strip_keyword_prefix("exclude,*.tmp", "exclude"),
            Some("*.tmp")
        );
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

    // A `\` in a peer-requested path is an ordinary filename byte on Unix, not
    // a separator. Upstream's `sanitize_path` (util1.c) tests `== '/'` at every
    // separator check and has no `\` arm, so it copies the byte through as part
    // of the component. oc used to split on it, which relocated the file: with
    // `a` present the request landed at `a/b`, and without it the open failed
    // ENOENT and the connection died. MEASURED against real upstream 3.5.0.
    // upstream main.c:741 `get_local_name()`: `trailing_slash = cp && !cp[1]`,
    // and `file_total > 1 || trailing_slash` takes the make-a-directory branch
    // (mkdir + chdir + NULL local_name), so a single source file lands INSIDE.
    // Without the slash the dest names the file itself. oc's local path already
    // honours that rule; the daemon was dropping the slash before the receiver
    // could see it, so a peer asking for a DIRECTORY silently got a FILE.
    //
    // ⚠ Asserts on the raw OsStr, NOT on Path equality: `Path::new("a/") ==
    // Path::new("a")` is TRUE because Path compares components and discards a
    // trailing separator. A Path-equality assertion here would pass both before
    // and after the fix - it cannot see the thing under test.
    #[test]
    fn resolve_receiver_dest_preserves_a_trailing_slash() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/realdir/".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(
            dest.as_os_str(),
            std::ffi::OsStr::new("/srv/upload/realdir/")
        );
    }

    // Non-vacuity companion: the SAME tail without the slash must NOT gain one,
    // or the fix would just be appending a slash unconditionally.
    //
    // The expectation is built with `join` rather than spelled out, because this
    // arm returns `module_path.join(collapsed)` and `join` inserts the PLATFORM
    // separator - a literal "/srv/upload/realdir" would be right on Unix and
    // wrong on Windows, where `join` yields a backslash. Comparing against the
    // same construction keeps the assertion about the trailing separator, which
    // is what the test is for.
    #[test]
    fn resolve_receiver_dest_does_not_invent_a_trailing_slash() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/realdir".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest.as_os_str(), module_path.join("realdir").as_os_str());
    }

    // The slash must survive `..` collapsing, which is what strips it.
    #[test]
    fn resolve_receiver_dest_keeps_the_slash_through_a_dotdot_collapse() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/x/../y/".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest.as_os_str(), std::ffi::OsStr::new("/srv/upload/y/"));
    }

    #[test]
    fn resolve_receiver_dest_keeps_a_backslash_as_a_filename_byte() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/a\\b".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        // The whole `a\b` is ONE component. Splitting would give `/srv/upload/a/b`.
        assert_eq!(dest, std::path::Path::new("/srv/upload/a\\b"));
    }

    #[test]
    fn resolve_receiver_dest_keeps_a_trailing_backslash() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/sub\\".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        // Treating the trailing `\` as a separator silently stripped it,
        // yielding `/srv/upload/sub` for a peer that asked for `sub\`.
        assert_eq!(dest, std::path::Path::new("/srv/upload/sub\\"));
    }

    // Non-vacuity companion: `/` MUST still split, and `..` must still collapse
    // per upstream's depth-0 sanitize_path. Without this a fix that stopped
    // splitting on every separator would also pass the two tests above.
    #[test]
    fn resolve_receiver_dest_still_splits_and_collapses_on_slash() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/x/../y/z".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest, std::path::Path::new("/srv/upload/y/z"));
    }

    // A `\` must not smuggle a traversal past the `..` collapse either: the
    // collapse sees ONE component named `..\..`, which is not `..`, so it is
    // kept as a literal name rather than popping two levels.
    #[test]
    fn resolve_receiver_dest_backslash_does_not_form_a_dotdot_component() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/..\\..\\etc".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest, std::path::Path::new("/srv/upload/..\\..\\etc"));
        assert!(dest.starts_with(module_path));
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

    #[test]
    fn resolve_receiver_dest_collapses_parent_dir_under_module_root() {
        // upstream util1.c:1183 with depth 0: a `..` with nothing to pop is
        // DISCARDED, not refused, so the escape clamps at the module root and
        // is served. Refusing here rejected a request both 3.4.4 and 3.5.0
        // accept.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/../../etc/passwd".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest, std::path::Path::new("/srv/upload/etc/passwd"));
    }

    #[test]
    fn resolve_receiver_dest_collapses_in_tree_parent_dir() {
        // The case the old reject broke for legitimate clients: `a/../b` is an
        // ordinary in-tree path that never leaves the module.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/a/../b".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest, std::path::Path::new("/srv/upload/b"));
    }

    #[test]
    fn resolve_receiver_dest_collapses_to_module_root_when_fully_consumed() {
        // Every component popped: upstream's `if (sanp == dest) *sanp++ = '.'`
        // (util1.c:1205) yields the module root itself.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/a/../..".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(dest, module_path);
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
        // suite on master. upstream `main.c:867 check_alt_basis_dirs` warns
        // on a missing/out-of-tree basis but never aborts.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let outside_root = outside.path().canonicalize().expect("canonicalise outside");

        // upstream rewrites rather than refuses: util1.c:1145-1152 re-roots an
        // absolute arg at `module_dir` with depth forced to 0, so the basis
        // lands under the module and check_alt_basis_dirs then warns that it
        // does not exist. The out-of-module directory is never reached.
        let clamped = clamp_basis_to_module(&outside_root, &module_root, &module_root);
        assert!(
            clamped.starts_with(&module_root),
            "absolute out-of-module basis must be clamped under the module, got {clamped:?}",
        );
        assert!(
            !clamped.exists(),
            "the clamped basis must not resolve to a real out-of-tree dir"
        );
    }

    #[test]
    fn confine_basis_re_roots_absolute_in_module_the_way_upstream_does() {
        // An ABSOLUTE basis is re-rooted at the module unconditionally -
        // upstream `util1.c:1145-1152` takes the `*p == '/'` branch, sets
        // `rootdir = module_dir` and `depth = 0`, with no
        // already-under-the-root special case. So even a path that already
        // names an in-module directory gets the module prefix a second time
        // and then fails to exist.
        //
        // MEASURED against real 3.5.0 over a daemon push, not inferred: with
        // module `<B>/mod` and `--link-dest=<B>/mod/snap` (a directory that
        // really exists), upstream prints
        //   `--link-dest arg does not exist: <B>/mod<B>/mod/snap`
        // and oc now prints the same bytes. Addressing an in-module basis
        // from a daemon client therefore requires the RELATIVE spelling -
        // which is what the sibling `../01` tests cover.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let in_module = module_root.join("snap");
        std::fs::create_dir(&in_module).expect("create in-module snap dir");

        let resolved = clamp_basis_to_module(&in_module, &module_root, &module_root);

        // Asserted as OBSERVABLE PROPERTIES, not by recomputing the component
        // walk: re-deriving the expected value with the same rule the function
        // uses would pass for any rule at all. Spelling the root prefix by hand
        // (`strip_prefix("/")`) is not portable either - an absolute path is
        // `C:\...` on Windows, so that form panicked there while passing on
        // Unix.
        assert!(
            resolved.starts_with(&module_root),
            "the clamp must stay under the module root, got {resolved:?}",
        );
        assert_ne!(
            resolved, in_module,
            "an absolute basis is RE-ROOTED, not passed through - that is the \
             whole divergence this test pins",
        );
        assert_eq!(
            resolved.file_name(),
            in_module.file_name(),
            "re-rooting preserves the components, it only moves them",
        );
        assert!(
            !resolved.exists(),
            "the re-rooted path names nothing, which is why upstream warns \
             `arg does not exist` even though `{in_module:?}` really exists",
        );
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
        let clamped = clamp_basis_to_module(&escape, &module_root, &module_root);
        assert!(
            clamped.starts_with(&module_root),
            "absolute `..` escape must be clamped under the module, got {clamped:?}",
        );
        assert_ne!(clamped, sibling, "the real sibling must never be reached");
    }

    #[test]
    fn confine_basis_joins_relative_under_resolve_base() {
        // Relative basis paths still resolve under the receiver's dest dir
        // (the `resolve_base`), matching upstream `main.c:1230-1241`
        // post-`get_local_name` chdir behaviour. This pins the legacy
        // relative branch so the absolute-path extension doesn't regress it.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let dest = module_root.join("00");
        std::fs::create_dir(&dest).expect("create dest 00");
        let sibling = module_root.join("01");
        std::fs::create_dir(&sibling).expect("create sibling 01");

        let resolved = clamp_basis_to_module(std::path::Path::new("../01"), &dest, &module_root);
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
    // `util1.c:1138 sanitize_path` collapses `..` against the module root
    // depth (rewriting the path under the module) and `main.c:867
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

        // Upstream's depth-0 sanitize DISCARDS the unpoppable `..` rather than
        // refusing, so the basis becomes the in-module `etc/passwd` - which
        // does not exist, drawing the `arg does not exist` warning. The point
        // is that `<module_parent>/etc/passwd` is never addressed.
        let clamped = clamp_basis_to_module(escape, &resolve_base, &module_root);
        assert_eq!(clamped, module_root.join("etc/passwd"));
        assert!(
            clamped.starts_with(&module_root),
            "relative climb past the module root must be clamped, got {clamped:?}",
        );
    }

    #[test]
    fn confine_basis_link_dest_relative_in_module_sibling_is_accepted() {
        // Positive control paired with the `../etc/passwd` negative case
        // above. A relative basis that resolves to an in-module sibling
        // must survive so operator-permitted snapshot layouts (e.g. the
        // upstream `dest/00 + --link-dest=../01` pattern from
        // main.c:885-898) still hard-link instead of re-transferring.
        let module = tempfile::TempDir::new().expect("module tempdir");
        let module_root = module.path().canonicalize().expect("canonicalise module");
        let dest = module_root.join("00");
        std::fs::create_dir(&dest).expect("create dest 00");
        let sibling = module_root.join("01");
        std::fs::create_dir(&sibling).expect("create sibling 01");

        let resolved = clamp_basis_to_module(std::path::Path::new("../01"), &dest, &module_root);
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
        // The clamp is LEXICAL, so `cd` stays module-relative on paper; the
        // separate symlink-resolution rule is what refuses it. Asserting both
        // halves keeps a future simplification from collapsing them: a clamp
        // alone would admit the escape.
        let clamped = clamp_basis_to_module(std::path::Path::new("cd"), &module_root, &module_root);
        assert_eq!(
            clamped, trap,
            "the lexical clamp alone does NOT refuse this"
        );
        assert!(
            basis_resolves_outside_module(&clamped, &module_root),
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

        let clamped = clamp_basis_to_module(&outside, &module_root, &module_root);
        assert!(
            clamped.starts_with(&module_root),
            "absolute --link-dest pointing outside the module root must be clamped, got {clamped:?}",
        );
        assert!(
            !clamped.exists(),
            "the clamped basis must not name the real outside file"
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
        let sources = resolve_sender_sources(module_path, &args, "upload");
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
        let sources = resolve_sender_sources(module_path, &args, "upload");
        assert_eq!(sources, vec![std::path::PathBuf::from("/srv/upload/")]);
    }

    #[test]
    fn resolve_sender_sources_joins_single_file_subpath_with_module_root() {
        // upstream: flist.c:2338-2349 - a single-file sub-path positional is
        // joined with module_path so the sender walks exactly that one path
        // and the per-positional dir/fn split emits the basename.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/d1/d2/f2".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload");
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
        let sources = resolve_sender_sources(module_path, &args, "upload");
        let lossy: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(lossy, vec!["/srv/upload/d1/d2/".to_owned()]);
    }

    // Upstream's `sanitize_path` copies each component THROUGH its trailing
    // slash (util1.c:1201) and only then examines the next one, so discarding a
    // `.` (util1.c:1163-1172) leaves that slash in the output: `d1/.` sanitizes
    // to `d1/`, not `d1`. The surviving slash is upstream's DOTDIR marker
    // (flist.c:2589-2594), and it is the `name_type != NORMAL_NAME` disjunct of
    // `link_stat(fbuf, &st, copy_dirlinks || name_type != NORMAL_NAME)`
    // (flist.c:2696) - so losing it makes a symlinked directory ship as a
    // symlink instead of its contents. oc split on `/`, dropped the `.` segment
    // AND its separator, and re-attached a slash only when the RAW tail ended in
    // one, which `d1/.` does not.
    //
    // ⚠ Asserts on the lossy STRING, not on PathBuf equality: `Path::new("a/")
    // == Path::new("a")` is TRUE because Path compares components and discards
    // a trailing separator, so a PathBuf assertion would pass both before and
    // after the fix and could not see the thing under test.
    #[test]
    fn resolve_sender_sources_keeps_the_dotdir_marker_on_a_trailing_dot() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/d1/d2/.".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload");
        let lossy: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(lossy, vec!["/srv/upload/d1/d2/".to_owned()]);
    }

    // Same rule reached through the `..` arm: upstream backs `sanp` up to just
    // past the previous separator (util1.c:1186-1190), so `d1/d2/..` also ends
    // in a slash and also carries the marker.
    #[test]
    fn resolve_sender_sources_keeps_the_dotdir_marker_through_a_dotdot_collapse() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/d1/d2/..".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload");
        let lossy: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(lossy, vec!["/srv/upload/d1/".to_owned()]);
    }

    // Non-vacuity companion for the two cells above: the marker must be EARNED,
    // never appended. Its verdict does not depend on the fix, so a green
    // companion beside a red pin proves the assertion is about the marker and
    // not about a resolver that decorates every source with a slash.
    #[test]
    fn resolve_sender_sources_does_not_invent_a_dotdir_marker() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/d1/d2".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload");
        let lossy: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(lossy, vec!["/srv/upload/d1/d2".to_owned()]);
    }

    // The filed claim for the second defect was that every `.` operand is
    // stripped and nothing transfers. It is not: a lone `.` is short-circuited
    // to the module root WITH the marker before the collapse is ever reached.
    // Pinned so the claim cannot become true later.
    #[test]
    fn resolve_sender_sources_bare_dot_operand_keeps_the_module_root_marker() {
        let module_path = std::path::Path::new("/srv/upload");
        for tail in ["upload/.", "upload/./", "upload/./."] {
            let args = vec![".".to_owned(), tail.to_owned()];
            let sources = resolve_sender_sources(module_path, &args, "upload");
            let lossy: Vec<String> = sources
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                lossy,
                vec!["/srv/upload/".to_owned()],
                "`{tail}` names the module root with upstream's DOTDIR marker \
                 (flist.c:2601-2604); an empty list is the stripped-to-nothing \
                 shape the note claimed"
            );
        }
    }

    // Receiver-side counterpart of the sender pin: the same `sanitize_path`
    // rule feeds `get_local_name()`'s `trailing_slash` (main.c:741), which
    // decides whether a single incoming file lands INSIDE the destination or
    // renames it.
    #[test]
    fn resolve_receiver_dest_keeps_the_slash_through_a_trailing_dot_dir() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/realdir/.".to_owned()];
        let dest = resolve_receiver_dest(module_path, &args, "upload");
        assert_eq!(
            dest.as_os_str(),
            std::ffi::OsStr::new("/srv/upload/realdir/")
        );
    }

    #[test]
    fn resolve_sender_sources_collapses_parent_dir_under_module_root() {
        // The escape clamps at the module root instead of being refused, which
        // is what upstream serves. Containment still holds: the resolved source
        // is inside `/srv/upload`, so a chroot-less daemon leaks nothing.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/../etc/passwd".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload");
        assert_eq!(
            sources,
            vec![std::path::PathBuf::from("/srv/upload/etc/passwd")]
        );
    }

    #[test]
    fn resolve_sender_sources_collapses_mid_path_parent_dir() {
        // `d1/../../secret`: `d1` is popped by the first `..`, the second has
        // nothing to pop and is discarded - upstream util1.c:1183-1191.
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload/d1/../../secret".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload");
        assert_eq!(
            sources,
            vec![std::path::PathBuf::from("/srv/upload/secret")]
        );
    }

    #[test]
    fn resolve_sender_sources_collapse_never_escapes_the_module_root() {
        // CLASS assertion, not a single case: no amount of `..` may produce a
        // path outside the module root. This is the security property the old
        // reject was protecting, restated so the collapse must uphold it.
        let module_path = std::path::Path::new("/srv/upload");
        for tail in [
            "upload/..",
            "upload/../..",
            "upload/../../../../../../etc/shadow",
            "upload/a/../../../b",
            "upload/./../.././x",
        ] {
            let args = vec![".".to_owned(), tail.to_owned()];
            for src in resolve_sender_sources(module_path, &args, "upload") {
                assert!(
                    src.starts_with(module_path),
                    "{tail} resolved to {src:?}, which escapes {module_path:?}"
                );
            }
        }
    }

    #[test]
    fn resolve_sender_sources_strips_leading_slash_before_join() {
        let module_path = std::path::Path::new("/srv/upload");
        let args = vec![".".to_owned(), "upload//d1/d2/f2".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload");
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
        let sources = resolve_sender_sources(module_path, &args, "mod");
        assert_eq!(sources, vec![module_path.join("foo")]);
    }

    #[test]
    fn resolve_sender_sources_glob_keeps_literal_when_no_match() {
        // upstream: util1.c:864 - `glob.argc == save_argc` branch preserves
        // the literal arg when nothing matches so the sender surfaces a
        // normal link_stat failure (exit 23) instead of dropping silently.
        let tmp = tempfile::tempdir().expect("tempdir");
        let module_path = tmp.path();
        std::fs::create_dir(module_path.join("bar")).expect("bar dir");

        let args = vec![".".to_owned(), "mod/z*".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "mod");
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
        let sources = resolve_sender_sources(module_path, &args, "mod");
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
        let mut sources = resolve_sender_sources(module_path, &args, "mod");
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
        let sources = resolve_sender_sources(module_path, &args, "mod");
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
        let sources = resolve_sender_sources(module_path, &args, "mod");
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
        let sources = resolve_sender_sources(module_path, &args, "upload");
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
        let sources = resolve_sender_sources(module_path, &args, "upload");
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
        let sources = resolve_sender_sources(module_path, &args, "upload");
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
    fn resolve_sender_sources_collapses_parent_dir_on_windows() {
        // The collapse must behave identically on every host, and must still
        // join with a literal `/` (upstream pathjoin) rather than `\`, because
        // the result is compared against module-relative wire names.
        let module_path = std::path::Path::new(r"C:\srv\upload");
        let args = vec![".".to_owned(), "upload/../etc/passwd".to_owned()];
        let sources = resolve_sender_sources(module_path, &args, "upload");
        assert_eq!(
            sources,
            vec![std::path::PathBuf::from(r"C:\srv\upload/etc/passwd")]
        );
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
        let dest = resolve_receiver_dest(module_path, &args, "upload");
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
        let dest = resolve_receiver_dest(module_path, &args, "upload");
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
        assert!(access_effective_read_only(false, UserAccessLevel::ReadOnly));
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
        assert!(!access_effective_read_only(false, UserAccessLevel::Default));
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

    // upstream: loadparm `open noatime` - the module directive makes the daemon
    // (as sender) open source files with O_NOATIME. Without wiring it into the
    // server config the directive was parsed but never enforced.
    #[test]
    fn module_open_noatime_forces_config_flag() {
        let module = ModuleDefinition {
            open_noatime: true,
            ..Default::default()
        };
        let mut cfg = ServerConfig::default();
        assert!(!cfg.write.open_noatime);
        apply_module_transfer_directives(&module, &mut cfg);
        assert!(cfg.write.open_noatime);
    }

    #[test]
    fn module_without_open_noatime_leaves_config_untouched() {
        let module = ModuleDefinition::default();
        let mut cfg = ServerConfig::default();
        apply_module_transfer_directives(&module, &mut cfg);
        assert!(!cfg.write.open_noatime);
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

/// Pins upstream's `MAX_DAEMON_ARGS` ceiling on the phase-1 argument read.
///
/// upstream: `io.c:1476-1479` - `if (mod_name && argc >= MAX_DAEMON_ARGS - 1)`
/// then `rprintf(FERROR, "too many daemon arguments\n")` and
/// `exit_cleanup(RERR_PROTOCOL)`.
#[cfg(test)]
mod daemon_argv_limit_tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    fn null_terminated_args(count: usize) -> Vec<u8> {
        let mut buf = Vec::with_capacity(count * 2 + 1);
        for _ in 0..count {
            buf.extend_from_slice(b"a\0");
        }
        buf.push(0);
        buf
    }

    #[test]
    fn read_client_arguments_refuses_past_the_daemon_ceiling() {
        let input = null_terminated_args(protocol::secluded_args::MAX_DAEMON_ARGS + 10);
        let mut reader = BufReader::new(Cursor::new(input));

        let err = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect_err("the daemon must refuse an unbounded argument vector");

        assert_eq!(err.to_string(), "too many daemon arguments");
    }

    /// Non-vacuity companion: an ordinary vector still reads. Without it the
    /// refusal above would also pass if the reader had stopped working.
    #[test]
    fn read_client_arguments_still_accepts_an_ordinary_vector() {
        let input = null_terminated_args(64);
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("an ordinary argument vector must still be read");

        assert_eq!(args.len(), 64);
    }
}

/// Pins what a peer can and cannot get appended to the operator's log file
/// through the daemon's per-request line.
///
/// upstream: io.c:1486-1495 - `request` is assembled from the argv entries
/// AFTER the `.` cwd marker, so a client's OPTION args never reach the log at
/// any verbosity, while its file operands do, exactly as upstream writes them.
#[cfg(test)]
mod daemon_client_arg_logging_tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));

    fn line_for(args: &[&str]) -> String {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let request = daemon_request(&owned).expect("an operand follows the marker");
        daemon_request_log_line(
            &request,
            ServerRole::Generator,
            None,
            "client.example",
            PEER,
        )
    }

    #[test]
    fn option_arguments_never_reach_the_log_line() {
        let payload = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let line = line_for(&["--server", payload, "-e.LsfxCIu", ".", "data/f"]);

        assert!(
            !line.contains(payload),
            "a peer-supplied option argument reached the daemon log: {line}"
        );
        assert!(
            !line.contains("--server"),
            "a peer-supplied option argument reached the daemon log: {line}"
        );
        assert_eq!(line, "rsync on data/f from client.example (203.0.113.7)");
    }

    /// Non-vacuity companion: the operand IS carried, so the assertions above
    /// are not passing merely because the line is constant text.
    #[test]
    fn the_operand_is_carried_verbatim() {
        assert_eq!(
            line_for(&["--server", ".", "data/other"]),
            "rsync on data/other from client.example (203.0.113.7)",
        );
    }
}
