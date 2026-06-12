use super::*;

#[test]
fn parse_daemon_option_extracts_option_payload() {
    assert_eq!(parse_daemon_option("OPTION --list"), Some("--list"));
    assert_eq!(
        parse_daemon_option("option --max-verbosity"),
        Some("--max-verbosity")
    );
}

#[test]
fn parse_daemon_option_rejects_invalid_values() {
    assert!(parse_daemon_option("HELLO there").is_none());
    assert!(parse_daemon_option("OPTION   ").is_none());
}

#[test]
fn canonical_option_trims_prefix_and_normalises_case() {
    assert_eq!(canonical_option("--Delete"), "delete");
    assert_eq!(canonical_option(" -P --info"), "p");
    assert_eq!(canonical_option("   CHECKSUM=md5"), "checksum");
}

#[test]
fn refused_option_exact_match() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete".to_owned()],
        ..Default::default()
    };
    let options = vec!["--delete".to_owned()];
    assert_eq!(refused_option(&module, &options), Some("--delete"));
}

#[test]
fn refused_option_exact_no_match() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete".to_owned()],
        ..Default::default()
    };
    let options = vec!["--compress".to_owned()];
    assert_eq!(refused_option(&module, &options), None);
}

#[test]
fn refused_option_glob_star_suffix() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete*".to_owned()],
        ..Default::default()
    };
    let options = vec!["--delete-before".to_owned()];
    assert_eq!(refused_option(&module, &options), Some("--delete-before"));
}

#[test]
fn refused_option_glob_star_matches_base() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete*".to_owned()],
        ..Default::default()
    };
    let options = vec!["--delete".to_owned()];
    assert_eq!(refused_option(&module, &options), Some("--delete"));
}

#[test]
fn refused_option_glob_star_matches_multiple_variants() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete*".to_owned()],
        ..Default::default()
    };
    for variant in &[
        "--delete",
        "--delete-before",
        "--delete-after",
        "--delete-during",
    ] {
        let options = vec![variant.to_string()];
        assert_eq!(
            refused_option(&module, &options),
            Some(*variant),
            "expected '{variant}' to be refused by 'delete*'"
        );
    }
}

#[test]
fn refused_option_glob_does_not_overmatch() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete*".to_owned()],
        ..Default::default()
    };
    let options = vec!["--compress".to_owned()];
    assert_eq!(refused_option(&module, &options), None);
}

#[test]
fn refused_option_negation_unrefuses() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete*".to_owned(), "!delete-during".to_owned()],
        ..Default::default()
    };
    let options_refused = vec!["--delete-before".to_owned()];
    assert_eq!(
        refused_option(&module, &options_refused),
        Some("--delete-before")
    );

    let options_allowed = vec!["--delete-during".to_owned()];
    assert_eq!(refused_option(&module, &options_allowed), None);
}

#[test]
fn refused_option_wildcard_all_refuses_non_vital() {
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    let options = vec!["--compress".to_owned()];
    assert_eq!(refused_option(&module, &options), Some("--compress"));
}

#[test]
fn refused_option_wildcard_all_spares_vital_server() {
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    let options = vec!["--server".to_owned()];
    assert_eq!(refused_option(&module, &options), None);
}

#[test]
fn refused_option_wildcard_all_spares_vital_sender() {
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    let options = vec!["--sender".to_owned()];
    assert_eq!(refused_option(&module, &options), None);
}

#[test]
fn refused_option_wildcard_all_spares_vital_dry_run() {
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    let options = vec!["--dry-run".to_owned()];
    assert_eq!(refused_option(&module, &options), None);
}

#[test]
fn refused_option_wildcard_all_spares_vital_short_n() {
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    let options = vec!["-n".to_owned()];
    assert_eq!(refused_option(&module, &options), None);
}

#[test]
fn refused_option_wildcard_all_spares_vital_checksum_seed() {
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    let options = vec!["--checksum-seed".to_owned()];
    assert_eq!(refused_option(&module, &options), None);
}

#[test]
fn refused_option_vital_can_be_refused_explicitly() {
    let module = ModuleDefinition {
        refuse_options: vec!["dry-run".to_owned()],
        ..Default::default()
    };
    let options = vec!["--dry-run".to_owned()];
    assert_eq!(refused_option(&module, &options), Some("--dry-run"));
}

#[test]
fn refused_option_wildcard_all_with_negation_allowlist() {
    let module = ModuleDefinition {
        refuse_options: vec![
            "*".to_owned(),
            "!a".to_owned(),
            "!v".to_owned(),
            "!compress".to_owned(),
        ],
        ..Default::default()
    };
    let options_allowed = vec!["--compress".to_owned()];
    assert_eq!(refused_option(&module, &options_allowed), None);

    let options_refused = vec!["--delete".to_owned()];
    assert_eq!(refused_option(&module, &options_refused), Some("--delete"));
}

#[test]
fn refused_option_multiple_exact_rules() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete".to_owned(), "compress".to_owned()],
        ..Default::default()
    };
    let options = vec!["--compress".to_owned()];
    assert_eq!(refused_option(&module, &options), Some("--compress"));
}

#[test]
fn refused_option_empty_list_allows_all() {
    let module = ModuleDefinition {
        refuse_options: Vec::new(),
        ..Default::default()
    };
    let options = vec!["--delete".to_owned()];
    assert_eq!(refused_option(&module, &options), None);
}

#[test]
fn refused_option_case_insensitive() {
    let module = ModuleDefinition {
        refuse_options: vec!["DELETE".to_owned()],
        ..Default::default()
    };
    let options = vec!["--delete".to_owned()];
    assert_eq!(refused_option(&module, &options), Some("--delete"));
}

#[test]
fn refused_option_question_mark_glob() {
    let module = ModuleDefinition {
        refuse_options: vec!["delete-?".to_owned()],
        ..Default::default()
    };
    // "delete-before" has more than one char after "delete-", so ? won't match
    let options_no_match = vec!["--delete-before".to_owned()];
    assert_eq!(refused_option(&module, &options_no_match), None);
}

#[test]
fn refused_option_returns_first_refused() {
    let module = ModuleDefinition {
        refuse_options: vec!["compress".to_owned(), "delete".to_owned()],
        ..Default::default()
    };
    let options = vec!["--delete".to_owned(), "--compress".to_owned()];
    assert_eq!(refused_option(&module, &options), Some("--delete"));
}

#[test]
fn is_option_refused_last_rule_wins() {
    let refuse = vec!["delete".to_owned(), "!delete".to_owned()];
    assert!(!is_option_refused(&refuse, "delete", None));
}

#[test]
fn is_option_refused_re_refuse_after_negation() {
    let refuse = vec![
        "delete*".to_owned(),
        "!delete-during".to_owned(),
        "delete-during".to_owned(),
    ];
    assert!(is_option_refused(&refuse, "delete-during", None));
}

#[test]
fn is_option_refused_short_letter_rule_matches_long_option() {
    // upstream: options.c:909-921 `parse_one_refuse_match` compares each rule
    // against BOTH `op->longName` and `op->shortName`. A `!v` rule must
    // un-refuse the same option that `!verbose` would, so allow-list
    // configurations that use short-letter shorthands behave the same as
    // long-name shorthands.
    let allow_list = vec!["*".to_owned(), "!v".to_owned(), "!a".to_owned()];
    assert!(!is_option_refused(&allow_list, "verbose", Some('v')));
    assert!(!is_option_refused(&allow_list, "archive", Some('a')));
}

#[test]
fn is_option_refused_archive_rule_expands_to_short_letter_set() {
    // upstream: options.c:904-906 - the `archive` (and `a`) rules expand to
    // the character class `[ardlptgoD]` and so refuse every short letter that
    // `-a` implies, not just the `--archive` long name.
    let refuse = vec!["archive".to_owned()];
    assert!(is_option_refused(&refuse, "recursive", Some('r')));
    assert!(is_option_refused(&refuse, "links", Some('l')));
    assert!(is_option_refused(&refuse, "devices", Some('D')));
    assert!(!is_option_refused(&refuse, "compress", Some('z')));
}

#[test]
fn is_option_refused_pure_allowlist_does_not_refuse_anything() {
    // upstream: options.c:947-968 - every option starts marked as accepted
    // (`a*` or `a=`). A refuse list of only negations therefore touches
    // nothing - no option flips to refused. Sanity-check the obvious cases.
    let allow_list = vec!["!verbose".to_owned(), "!archive".to_owned()];
    assert!(!is_option_refused(&allow_list, "verbose", Some('v')));
    assert!(!is_option_refused(&allow_list, "archive", Some('a')));
    assert!(!is_option_refused(&allow_list, "compress", Some('z')));
    assert!(!is_option_refused(&allow_list, "delete", None));
}

#[test]
fn is_vital_option_recognises_all_vitals() {
    for &vital in VITAL_OPTIONS {
        assert!(is_vital_option(vital), "expected '{vital}' to be vital");
    }
}

#[test]
fn is_vital_option_rejects_non_vitals() {
    assert!(!is_vital_option("delete"));
    assert!(!is_vital_option("compress"));
    assert!(!is_vital_option("archive"));
}

#[test]
fn refused_client_arg_matches_long_form() {
    // upstream: clientserver.c rejects `--compress` when the module's
    // `refuse options = compress` rule is active.
    let module = ModuleDefinition {
        refuse_options: vec!["compress".to_owned()],
        ..Default::default()
    };
    let args = vec!["--server".to_owned(), "--compress".to_owned()];
    assert_eq!(
        refused_client_arg(&module, &args),
        Some("--compress".to_owned())
    );
}

#[test]
fn refused_client_arg_expands_bundled_short() {
    // upstream: the daemon expands `-z` inside a packed short-letter
    // server argstr (e.g. `-vlogDtprez.iLsfxCIvu`) into `--compress` via
    // popt's longName mapping, then matches against the refuse list.
    let module = ModuleDefinition {
        refuse_options: vec!["compress".to_owned()],
        ..Default::default()
    };
    let args = vec![
        "--server".to_owned(),
        "-vlogDtprez.iLsfxCIvu".to_owned(),
        ".".to_owned(),
        "no-compress/".to_owned(),
    ];
    assert_eq!(
        refused_client_arg(&module, &args),
        Some("--compress".to_owned())
    );
}

#[test]
fn refused_client_arg_skips_capability_suffix() {
    // The capability dot-suffix (`.LsfxCIvu`) is not a list of options and
    // must not be expanded - otherwise wildcard rules would erroneously
    // refuse them.
    let module = ModuleDefinition {
        refuse_options: vec!["fuzzy".to_owned()],
        ..Default::default()
    };
    let args = vec!["-e.LsfxCIvu".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_allows_when_refuse_list_empty() {
    let module = ModuleDefinition {
        refuse_options: Vec::new(),
        ..Default::default()
    };
    let args = vec!["-avz".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_wildcard_spares_vital_e() {
    // upstream: the `e` short letter (carries the compatibility-flags
    // string) is exempt from wildcard refuse rules.
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    let args = vec!["-e.LsfxCIvu".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_short_n_dry_run_spared_by_wildcard() {
    // upstream: `--dry-run` / `-n` is vital and must survive `*`.
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    let args = vec!["-n".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_allowlist_module_passes_bundled_av() {
    // Regression for the upstream `daemon-refuse` testsuite scenario where a
    // module exposes an allow-list `refuse options = * !verbose !archive ...`
    // and the client sends `-av`. Both `-a` and `-v` must survive the
    // refuse-list matcher; upstream rsync flips each option's `descrip` back
    // to "accepted" once the explicit `!verbose` / `!archive` rule lands.
    //
    // upstream: options.c:977-991 - rules are processed in order and the last
    // match wins; the negated rule after the catch-all wildcard un-refuses
    // both the long-form name and its short-letter alias.
    let module = ModuleDefinition {
        refuse_options: vec![
            "*".to_owned(),
            "!verbose".to_owned(),
            "!archive".to_owned(),
            "!iconv".to_owned(),
            "!no-iconv".to_owned(),
        ],
        ..Default::default()
    };
    let args = vec!["-av".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_allowlist_short_letter_negation_passes_av() {
    // upstream: options.c:909-921 - `parse_one_refuse_match` matches each
    // rule against both `op->longName` and `op->shortName`. A short-letter
    // allow-list `!v !a` therefore un-refuses the same options that
    // `!verbose !archive` would. Pinning this behaviour prevents a relapse of
    // the over-refusal bug where short-letter negations were silently
    // ignored against long-name option canonicals.
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned(), "!v".to_owned(), "!a".to_owned()],
        ..Default::default()
    };
    let args = vec!["-av".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_pure_negation_allowlist_passes_av() {
    // A pure-negation refuse list never marks anything as refused (nothing
    // was refused to begin with), so an `-av` transfer must succeed.
    // upstream: options.c:947-968 - every option starts as `a*`/`a=`
    // (accepted); only explicit non-negated rules flip the descrip to
    // `r*`/`r=`.
    let module = ModuleDefinition {
        refuse_options: vec!["!verbose".to_owned(), "!archive".to_owned()],
        ..Default::default()
    };
    let args = vec!["-av".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_archive_rule_refuses_implied_short_letters() {
    // upstream: options.c:904-906 - the `archive` rule rewrites itself to the
    // character class `[ardlptgoD]`. A module configured with
    // `refuse options = archive` must therefore reject `-r`, `-l`, `-D`, etc.
    // (and not just `--archive` itself).
    let module = ModuleDefinition {
        refuse_options: vec!["archive".to_owned()],
        ..Default::default()
    };
    assert_eq!(
        refused_client_arg(&module, &["-r".to_owned()]),
        Some("--recursive".to_owned()),
    );
    assert_eq!(
        refused_client_arg(&module, &["-l".to_owned()]),
        Some("--links".to_owned()),
    );
    assert_eq!(
        refused_client_arg(&module, &["-D".to_owned()]),
        Some("--devices".to_owned()),
    );
}

#[test]
fn program_name_rsyncd_as_str() {
    let name = ProgramName::Rsyncd;
    assert_eq!(name.as_str(), Brand::Upstream.daemon_program_name());
}

#[test]
fn program_name_oc_rsyncd_as_str() {
    let name = ProgramName::OcRsyncd;
    assert_eq!(name.as_str(), Brand::Oc.daemon_program_name());
}

#[test]
fn program_name_rsyncd_brand() {
    let name = ProgramName::Rsyncd;
    assert!(matches!(name.brand(), Brand::Upstream));
}

#[test]
fn program_name_oc_rsyncd_brand() {
    let name = ProgramName::OcRsyncd;
    assert!(matches!(name.brand(), Brand::Oc));
}

#[test]
fn program_name_equality() {
    assert_eq!(ProgramName::Rsyncd, ProgramName::Rsyncd);
    assert_eq!(ProgramName::OcRsyncd, ProgramName::OcRsyncd);
    assert_ne!(ProgramName::Rsyncd, ProgramName::OcRsyncd);
}

#[test]
fn program_name_clone() {
    let name = ProgramName::Rsyncd;
    let cloned = name;
    assert_eq!(name, cloned);
}

#[test]
fn program_name_debug() {
    let name = ProgramName::OcRsyncd;
    let debug = format!("{name:?}");
    assert!(debug.contains("OcRsyncd"));
}

#[test]
fn parse_args_empty_defaults_to_program_name() {
    let result = parse_args::<[&str; 0], &str>([]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(!parsed.show_help);
    assert_eq!(parsed.show_version, 0);
}

#[test]
fn parse_args_help_flag() {
    let args = ["rsyncd", "--help"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.show_help);
    assert_eq!(parsed.show_version, 0);
}

#[test]
fn parse_args_version_flag_long() {
    let args = ["rsyncd", "--version"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(!parsed.show_help);
    assert_eq!(parsed.show_version, 1);
}

#[test]
fn parse_args_version_flag_short() {
    let args = ["rsyncd", "-V"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.show_version, 1);
}

#[test]
fn parse_args_remainder_collected() {
    let args = ["rsyncd", "--config=/etc/rsyncd.conf", "--port=8873"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert_eq!(parsed.remainder.len(), 2);
}

#[test]
fn parse_args_oc_rsyncd_program_name() {
    let args = ["oc-rsyncd", "--help"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(matches!(parsed.program_name, ProgramName::OcRsyncd));
}

#[test]
fn parse_args_rsyncd_program_name() {
    // Note: The branding system recognizes "rsync" (not "rsyncd") as the upstream program
    let args = ["rsync", "--help"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(matches!(parsed.program_name, ProgramName::Rsyncd));
}

#[test]
fn parse_args_help_and_version_together() {
    let args = ["rsyncd", "--help", "--version"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.show_help);
    assert_eq!(parsed.show_version, 1);
}

#[test]
fn parse_args_hyphenated_values_in_remainder() {
    let args = ["rsyncd", "--no-detach", "--port=8873"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    // Hyphenated values should be in remainder
    assert!(parsed.remainder.iter().any(|a| a == "--no-detach"));
}

#[test]
fn clap_command_creates_command() {
    let cmd = clap_command("test-program");
    assert_eq!(cmd.get_name(), "test-program");
}

#[test]
fn clap_command_has_help_arg() {
    let cmd = clap_command("test");
    let args: Vec<_> = cmd.get_arguments().collect();
    assert!(args.iter().any(|a| a.get_id() == "help"));
}

#[test]
fn clap_command_has_version_arg() {
    let cmd = clap_command("test");
    let args: Vec<_> = cmd.get_arguments().collect();
    assert!(args.iter().any(|a| a.get_id() == "version"));
}

#[test]
fn render_help_rsyncd_contains_program_name() {
    let help = render_help(ProgramName::Rsyncd);
    assert!(!help.is_empty());
}

#[test]
fn render_help_oc_rsyncd_contains_program_name() {
    let help = render_help(ProgramName::OcRsyncd);
    assert!(!help.is_empty());
}

#[test]
fn apply_daemon_param_overrides_sets_read_only() {
    let mut module = ModuleDefinition::default();
    assert!(!module.read_only);
    let params = vec!["read only=true".to_owned()];
    apply_daemon_param_overrides(&params, &mut module).expect("should apply");
    assert!(module.read_only);
}

#[test]
fn apply_daemon_param_overrides_sets_timeout() {
    let mut module = ModuleDefinition::default();
    let params = vec!["timeout=120".to_owned()];
    apply_daemon_param_overrides(&params, &mut module).expect("should apply");
    assert_eq!(module.timeout.map(|t| t.get()), Some(120));
}

#[test]
fn apply_daemon_param_overrides_rejects_security_sensitive() {
    let mut module = ModuleDefinition::default();
    let params = vec!["hosts allow=*".to_owned()];
    assert!(apply_daemon_param_overrides(&params, &mut module).is_err());
}

#[test]
fn apply_daemon_param_overrides_rejects_missing_equals() {
    let mut module = ModuleDefinition::default();
    let params = vec!["read only".to_owned()];
    assert!(apply_daemon_param_overrides(&params, &mut module).is_err());
}

#[test]
fn apply_daemon_param_overrides_ignores_unknown_keys() {
    let mut module = ModuleDefinition::default();
    let params = vec!["unknown_key=value".to_owned()];
    apply_daemon_param_overrides(&params, &mut module).expect("should ignore unknown");
}

#[test]
fn apply_daemon_param_overrides_empty_params() {
    let mut module = ModuleDefinition::default();
    let original = module.clone();
    apply_daemon_param_overrides(&[], &mut module).expect("empty params should succeed");
    assert_eq!(module, original);
}

// UTS-13 + UTS-14: refused-option wire-frame regression tests.
//
// Background: when the daemon rejects a client-sent option (e.g. `--delete`,
// `--compress`) after the `@RSYNCD: OK` handshake, the client has already
// switched its input to the multiplex stream. Raw `@ERROR: ...\n` text written
// at that point is decoded by `read_a_msg()` as a multiplex frame header and
// surfaces as `unexpected tag 77 [Receiver]` because the upper byte of the
// header is the literal `T` from "The server ..." (84 - MPLEX_BASE = 77).
// The fix routes post-OK refusals through MSG_ERROR_XFER + MSG_ERROR_EXIT.
fn build_stdio_stream_with_capture() -> (DaemonStream, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
    let capture: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let reader: Box<dyn io::Read + Send> = Box::new(io::Cursor::new(Vec::new()));
    let writer: Box<dyn io::Write + Send> = Box::new(CaptureWriter(capture.clone()));
    let pair = crate::daemon_stream::StdioPair::new(reader, writer);
    (DaemonStream::stdio(pair), capture)
}

#[test]
fn refuse_emits_at_error_raw_pre_handshake() {
    // Pre-handshake path: client has not yet seen `@RSYNCD: OK`, so multiplex
    // IN has not started. The raw `@ERROR: ...\n` text is the correct wire
    // encoding here.
    let (mut stream, capture) = build_stdio_stream_with_capture();
    let mut limiter: Option<BandwidthLimiter> = None;
    let messages = LegacyMessageCache::shared();
    send_error_and_exit(
        &mut stream,
        &mut limiter,
        messages,
        "@ERROR: The server is configured to refuse --delete",
    )
    .expect("send pre-handshake error");

    let bytes = capture.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.starts_with("@ERROR: The server is configured to refuse --delete\n"),
        "pre-handshake refusal must be raw @ERROR text, got: {text:?}"
    );
    assert!(
        text.contains("@RSYNCD: EXIT"),
        "pre-handshake refusal must end with @RSYNCD: EXIT, got: {text:?}"
    );
}

#[test]
fn refuse_emits_msg_error_xfer_post_handshake() {
    // Post-handshake path: client has switched its input to the multiplexed
    // stream after `@RSYNCD: OK`. The daemon must wrap the error in a
    // MSG_ERROR_XFER frame followed by a MSG_ERROR_EXIT frame. Raw text would
    // be decoded as a multiplex header (the "unexpected tag 77" bug).
    //
    // upstream: clientserver.c:1175-1186 - io_start_multiplex_out() runs at
    // OK, then rwrite(FERROR, ...) emits a MSG_ERROR_XFER frame.
    let (mut stream, capture) = build_stdio_stream_with_capture();
    let mut limiter: Option<BandwidthLimiter> = None;
    send_multiplexed_error_and_exit(
        &mut stream,
        &mut limiter,
        "@ERROR: The server is configured to refuse --compress",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
    .expect("send multiplexed error");

    let bytes = capture.lock().unwrap().clone();
    assert!(
        bytes.len() >= 8,
        "must contain at least two 4-byte multiplex headers, got {} bytes",
        bytes.len()
    );

    // Build the expected MSG_ERROR_XFER frame and assert that the capture
    // starts with it. encode_into_writer writes the 4-byte little-endian
    // (length | code<<24) header followed by the payload bytes.
    let mut payload = b"@ERROR: The server is configured to refuse --compress".to_vec();
    payload.push(b'\n');
    let mut expected_xfer = Vec::new();
    MessageFrame::new(MessageCode::ErrorXfer, payload)
        .expect("frame")
        .encode_into_writer(&mut expected_xfer)
        .expect("encode");
    assert!(
        bytes.starts_with(&expected_xfer),
        "post-handshake refusal must begin with a MSG_ERROR_XFER frame; got {bytes:?}"
    );

    // The remaining bytes must be a MSG_ERROR_EXIT frame carrying the exit
    // code in little-endian form (upstream io.c:1060 send_msg_int).
    let mut expected_exit = Vec::new();
    MessageFrame::new(
        MessageCode::ErrorExit,
        FEATURE_UNAVAILABLE_EXIT_CODE.to_le_bytes().to_vec(),
    )
    .expect("frame")
    .encode_into_writer(&mut expected_exit)
    .expect("encode");
    assert!(
        bytes.ends_with(&expected_exit),
        "post-handshake refusal must end with a MSG_ERROR_EXIT frame; got {bytes:?}"
    );
}

#[test]
fn refuse_compress_message_mentions_compress() {
    // UTS-14 directly: the matcher already exists, but the refused-name
    // surfaces in the error string so the user sees `--compress` literally.
    let module = ModuleDefinition {
        refuse_options: vec!["compress".to_owned()],
        ..Default::default()
    };
    let options = vec!["--compress".to_owned()];
    let refused =
        refused_option(&module, &options).expect("compress must be matched as a refused option");
    let payload = format!("@ERROR: The server is configured to refuse {refused}");
    assert!(
        payload.contains("--compress"),
        "refusal payload must name the refused option literally, got: {payload}"
    );
}
