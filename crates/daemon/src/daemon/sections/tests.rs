use super::*;

/// Builds a module whose only non-default field is its `refuse options` list.
fn module_with_refuse(refuse_options: Vec<String>) -> ModuleDefinition {
    ModuleDefinition {
        refuse_options,
        ..Default::default()
    }
}

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

/// `canonical_option` strips the dashes and any `=value`, and PRESERVES case.
///
/// upstream matches refuse rules with `wildmatch` (options.c:923-924), not the
/// case-folding `iwildmatch` (lib/wildmatch.c:307-318). This test previously
/// asserted `"--Delete" -> "delete"` and pinned the folding.
#[test]
fn canonical_option_trims_prefix_and_preserves_case() {
    assert_eq!(canonical_option("--Delete"), "Delete");
    assert_eq!(canonical_option(" -P --info"), "P");
    assert_eq!(canonical_option("   CHECKSUM=md5"), "CHECKSUM");
    assert_eq!(canonical_option("--delete"), "delete");
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

/// Refuse matching is case-SENSITIVE, mirroring upstream.
///
/// upstream: `options.c:923-924` uses `wildmatch`; the case-folding
/// `iwildmatch` (lib/wildmatch.c:307-318, `force_lower_case = 1`) exists and is
/// deliberately not used, and no `tolower`/`strcasecmp` appears in the refuse
/// path.
///
/// This test previously asserted the OPPOSITE - that `refuse options = DELETE`
/// refuses `--delete` - and so pinned the divergence. The folding was not
/// merely cosmetic: it merged distinct options, so `refuse options = O`
/// (--omit-dir-times) also refused `-o` (--owner) and broke every `-a`
/// transfer against that module.
#[test]
fn refused_option_is_case_sensitive() {
    let upper_rule = module_with_refuse(vec!["DELETE".to_owned()]);
    assert_eq!(
        refused_option(&upper_rule, &["--delete".to_owned()]),
        None,
        "an upper-case rule must not match the lower-case option name"
    );
    assert_eq!(
        refused_option(&upper_rule, &["--DELETE".to_owned()]),
        Some("--DELETE"),
        "it must still match its own spelling"
    );

    // The load-bearing case: `-O` and `-o` are separate long_options[] rows.
    let omit_dir_times = module_with_refuse(vec!["O".to_owned()]);
    assert_eq!(
        refused_option(&omit_dir_times, &["-O".to_owned()]),
        Some("-O"),
        "`refuse options = O` must refuse --omit-dir-times"
    );
    assert_eq!(
        refused_option(&omit_dir_times, &["-o".to_owned()]),
        None,
        "`refuse options = O` must NOT refuse --owner - that broke -a transfers"
    );
}

/// Vitality belongs to one table row, so it must not leak across letter case.
///
/// `-n` (dry-run) is vital and immune to `refuse options = *`; `-N` (crtimes)
/// is a different row and is not. The previous case-folded vital lookup let
/// `-N` inherit `-n`'s immunity, so a wildcard rule silently spared it -
/// under-refusal, the fail-open direction.
#[test]
fn wildcard_all_refuses_crtimes_but_spares_dry_run() {
    let module = module_with_refuse(vec!["*".to_owned()]);
    assert_eq!(
        refused_option(&module, &["-n".to_owned()]),
        None,
        "-n (dry-run) is vital and survives a wildcard rule"
    );
    assert_eq!(
        refused_option(&module, &["-N".to_owned()]),
        Some("-N"),
        "-N (crtimes) is NOT vital and must be refused by a wildcard rule"
    );
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
    let module = module_with_refuse(vec!["delete".to_owned(), "!delete".to_owned()]);
    assert!(!is_option_refused(&module, "delete", None));
}

#[test]
fn is_option_refused_re_refuse_after_negation() {
    let module = module_with_refuse(vec![
        "delete*".to_owned(),
        "!delete-during".to_owned(),
        "delete-during".to_owned(),
    ]);
    assert!(is_option_refused(&module, "delete-during", None));
}

#[test]
fn is_option_refused_short_letter_rule_matches_long_option() {
    // upstream: options.c:921-933 `parse_one_refuse_match` compares each rule
    // against BOTH `op->longName` and `op->shortName`. A `!v` rule must
    // un-refuse the same option that `!verbose` would, so allow-list
    // configurations that use short-letter shorthands behave the same as
    // long-name shorthands.
    let module = module_with_refuse(vec!["*".to_owned(), "!v".to_owned(), "!a".to_owned()]);
    assert!(!is_option_refused(&module, "verbose", Some('v')));
    assert!(!is_option_refused(&module, "archive", Some('a')));
}

#[test]
fn is_option_refused_archive_rule_expands_to_short_letter_set() {
    // upstream: options.c:916-918 - the `archive` (and `a`) rules expand to
    // the character class `[ardlptgoD]` and so refuse every short letter that
    // `-a` implies, not just the `--archive` long name.
    let module = module_with_refuse(vec!["archive".to_owned()]);
    assert!(is_option_refused(&module, "recursive", Some('r')));
    assert!(is_option_refused(&module, "links", Some('l')));
    assert!(is_option_refused(&module, "devices", Some('D')));
    assert!(!is_option_refused(&module, "compress", Some('z')));
}

#[test]
fn is_option_refused_pure_allowlist_does_not_refuse_anything() {
    // upstream: options.c:959-980 - every option starts marked as accepted
    // (`a*` or `a=`). A refuse list of only negations therefore touches
    // nothing - no option flips to refused. Sanity-check the obvious cases.
    let module = module_with_refuse(vec!["!verbose".to_owned(), "!archive".to_owned()]);
    assert!(!is_option_refused(&module, "verbose", Some('v')));
    assert!(!is_option_refused(&module, "archive", Some('a')));
    assert!(!is_option_refused(&module, "compress", Some('z')));
    assert!(!is_option_refused(&module, "delete", None));
}

#[test]
fn is_vital_option_recognises_all_vitals() {
    for &vital in VITAL_OPTIONS {
        assert!(is_vital_option(vital), "expected '{vital}' to be vital");
    }
}

#[test]
fn default_refuses_client_log_file_options() {
    // upstream: options.c:1010 - a daemon always appends `log-file*` to the
    // refuse list after the module's own rules, refusing both `--log-file` and
    // `--log-file-format` so a client cannot redirect the server's logging.
    let module = ModuleDefinition::default();
    assert_eq!(
        refused_option(&module, &["--log-file".to_owned()]),
        Some("--log-file")
    );
    assert_eq!(
        refused_option(&module, &["--log-file-format".to_owned()]),
        Some("--log-file-format")
    );
}

#[test]
fn default_refuses_client_sent_insecure_links() {
    // upstream: options.c:1084 - a client must never be able to switch off the
    // daemon's symlink confinement. `--insecure-links` is a local-only opt-out
    // that upstream never forwards (options.c:3068), so one that arrives anyway
    // (injected with `-M`) is refused outright, dropping the connection. The
    // daemon's own opt-out is the `insecure links` module parameter.
    let module = ModuleDefinition::default();
    assert_eq!(
        refused_option(&module, &["--insecure-links".to_owned()]),
        Some("--insecure-links")
    );
}

#[test]
fn insecure_links_refusal_survives_negation() {
    // The refusal is appended AFTER the module's own `refuse options` rules
    // (options.c:1077-1088), so a `!insecure-links` negation cannot re-enable
    // it. Without this the module owner could hand the switch to the client.
    let module = module_with_refuse(vec!["!insecure-links".to_owned()]);
    assert_eq!(
        refused_option(&module, &["--insecure-links".to_owned()]),
        Some("--insecure-links")
    );
}

#[test]
fn insecure_links_refusal_does_not_catch_its_negated_spelling() {
    // Non-vacuity companion: `--no-insecure-links` RESTORES the check, so
    // refusing it would reject a request that only makes the daemon stricter.
    // MEASURED: relaxing the match to `contains("insecure-links")` kills exactly
    // this test and nothing else. (`starts_with` is an equivalent mutation here -
    // the negated spelling begins with `no-`, so no test distinguishes it.)
    let module = ModuleDefinition::default();
    assert_eq!(
        refused_option(&module, &["--no-insecure-links".to_owned()]),
        None
    );
}

#[test]
fn default_log_file_refusal_survives_negation() {
    // upstream: options.c:1005-1011 - the `log-file*` refusal is applied AFTER
    // the module's `refuse options` rules, so a `!log-file` negation cannot
    // re-enable it.
    let module = module_with_refuse(vec!["!log-file".to_owned()]);
    assert_eq!(
        refused_option(&module, &["--log-file".to_owned()]),
        Some("--log-file")
    );
}

#[test]
fn iconv_refused_when_module_has_no_charset() {
    // upstream: options.c:1007-1008 - `if (!*lp_charset(module_id))
    // parse_one_refuse_match(0, "iconv", ...)`: a daemon module with no
    // `charset` configured refuses client `--iconv`.
    let module = ModuleDefinition::default();
    assert_eq!(
        refused_option(&module, &["--iconv".to_owned()]),
        Some("--iconv")
    );
}

#[test]
fn iconv_allowed_when_module_configures_charset() {
    // upstream: options.c:1006-1009 - the iconv refusal is skipped when the
    // module sets a `charset`, so the client may negotiate `--iconv`.
    let module = ModuleDefinition {
        charset: Some("LATIN1".to_owned()),
        ..Default::default()
    };
    assert_eq!(refused_option(&module, &["--iconv".to_owned()]), None);
}

#[test]
fn wildcard_all_spares_vital_log_format_but_refuses_out_format() {
    // upstream: options.c:975 - the `log-format` long_options[] entry is
    // exact-match only (vital), so `refuse options = *` cannot refuse it, while
    // the wildcard-able `out-format` alias is refused by the same `*`.
    let module = module_with_refuse(vec!["*".to_owned()]);
    assert_eq!(refused_option(&module, &["--log-format".to_owned()]), None);
    assert_eq!(
        refused_option(&module, &["--out-format".to_owned()]),
        Some("--out-format")
    );
}

#[test]
fn exact_rule_refuses_vital_log_format() {
    // upstream: options.c:921-952 - a non-wild exact rule still marks a vital
    // option refused, so `refuse options = log-format` rejects `--log-format`.
    let module = module_with_refuse(vec!["log-format".to_owned()]);
    assert_eq!(
        refused_option(&module, &["--log-format".to_owned()]),
        Some("--log-format")
    );
}

#[test]
fn is_vital_option_rejects_non_vitals() {
    assert!(!is_vital_option("delete"));
    assert!(!is_vital_option("compress"));
    assert!(!is_vital_option("archive"));
}

/// A `[a-z]` RANGE in a refuse rule must match the range, not the three
/// literal bytes `a`, `-`, `z`.
///
/// upstream: `options.c:921-924` matches refuse rules with `wildmatch()`, and
/// `dowild` handles ranges at `lib/wildmatch.c:156-166`.
///
/// This is the fail-OPEN direction. oc's previous hand-written glob compared
/// `[...]` by literal byte membership, so `refuse options = --[a-c]*` accepted
/// `--compress` - the option the rule was written to block. Every other
/// refuse-options divergence over-refuses and so fails closed; this one did not.
#[test]
fn refused_client_arg_honors_a_character_range() {
    let module = ModuleDefinition {
        refuse_options: vec!["[a-c]*".to_owned()],
        ..Default::default()
    };
    let args = vec!["--server".to_owned(), "--compress".to_owned()];
    assert_eq!(
        refused_client_arg(&module, &args),
        Some("--compress".to_owned()),
        "a range rule must refuse an option inside the range"
    );

    // The range must still bound: an option outside it stays allowed.
    let allowed = vec!["--server".to_owned(), "--times".to_owned()];
    assert_eq!(
        refused_client_arg(&module, &allowed),
        None,
        "a range rule must not refuse an option outside the range"
    );
}

/// A NEGATED class `[!...]` must invert, per `dowild`.
///
/// upstream: `lib/wildmatch.c:139-143` - `special = p_ch == NEGATE_CLASS`.
/// The previous glob had no negation at all, so the leading `!` was compared
/// as a literal byte and the rule matched nothing.
#[test]
fn refused_client_arg_honors_a_negated_class() {
    let module = ModuleDefinition {
        refuse_options: vec!["[!x]*".to_owned()],
        ..Default::default()
    };
    let args = vec!["--server".to_owned(), "--compress".to_owned()];
    assert_eq!(
        refused_client_arg(&module, &args),
        Some("--compress".to_owned()),
        "a negated class must refuse an option whose first char is not excluded"
    );
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
    // server argstr into `--compress` via popt's longName mapping, then
    // matches against the refuse list.
    //
    // The letter order matters and is NOT arbitrary: upstream appends `z`
    // (options.c:2722-2723) and only then calls `maybe_add_e_option()`
    // (options.c:2728), so `e` is always LAST - everything after it is `-e`'s
    // argument. oc emits the same order (the capability suffix is appended
    // last, `invocation/builder.rs:233`). This fixture previously wrote
    // `...ez.iLsfxCIvu`, an order no conforming rsync produces.
    let module = ModuleDefinition {
        refuse_options: vec!["compress".to_owned()],
        ..Default::default()
    };
    let args = vec![
        "--server".to_owned(),
        "-vlogDtprze.iLsfxCIvu".to_owned(),
        ".".to_owned(),
        "no-compress/".to_owned(),
    ];
    assert_eq!(
        refused_client_arg(&module, &args),
        Some("--compress".to_owned())
    );
}

#[test]
fn refused_client_arg_scans_past_non_option_bytes() {
    // A non-option byte must NOT end the bundle scan. Upstream is
    // position-independent by construction: refusal rewrites `op->val` to
    // OPT_REFUSED_BASE+idx (options.c:1040) and popt returns the refused entry
    // (options.c:1934) wherever it sits.
    //
    // Breaking at the first non-alphabetic byte let a client prefix its bundle
    // with any digit and slip the rest past the refuse list SILENTLY - `-4z`
    // enabled compression with `refuse options = compress` in force.
    let module = ModuleDefinition {
        refuse_options: vec!["compress".to_owned()],
        ..Default::default()
    };
    for arg in ["-4z", "-6z", "-8z", "-0z", "-@z"] {
        assert_eq!(
            refused_client_arg(&module, &[arg.to_owned()]),
            Some("--compress".to_owned()),
            "bundle {arg} must not bypass the refuse list",
        );
    }
}

#[test]
fn refused_client_arg_examines_every_letter_the_decoder_acts_on() {
    // THE invariant while oc has two readers of a compact bundle: this scanner
    // must examine a SUPERSET of what the code that APPLIES the options acts
    // on. Anything less is a refusal bypass.
    //
    // The applying decoder is `transfer::flags::ParsedServerFlags::parse`,
    // which walks EVERY byte up to the `.` separator with no option-argument
    // (arity) logic. Upstream can afford arity because one popt pass both
    // parses and marks refusals, so they cannot disagree; oc's two readers can.
    //
    // Concretely: teaching only this scanner that `-B` takes a value made it
    // skip the rest of `-B...z`, while the decoder still saw the `z` and turned
    // compression on - a silent bypass strictly worse than the one it fixed.
    // Over-refusing a byte that is really part of an argument fails CLOSED and
    // is the acceptable direction.
    let module = ModuleDefinition {
        refuse_options: vec!["compress".to_owned()],
        ..Default::default()
    };
    // Every one of these has `z` AFTER a letter that upstream popt would treat
    // as value-taking. The decoder sets compress for all of them, so all must
    // be refused.
    for arg in ["-B4096z", "-T/tmpz", "-M--fooz", "-@z", "-fz"] {
        assert_eq!(
            refused_client_arg(&module, &[arg.to_owned()]),
            Some("--compress".to_owned()),
            "{arg}: decoder sets compress here, so the refuse scan must see it",
        );
    }

    // The one place the scan DOES stop is the `.` capability separator - and it
    // stops there because the decoder stops there too (flags.rs:531 `if byte ==
    // b'.' { break; }`). Same rule, same boundary.
    let fuzzy_module = ModuleDefinition {
        refuse_options: vec!["fuzzy".to_owned()],
        ..Default::default()
    };
    assert_eq!(
        refused_client_arg(&fuzzy_module, &["-e.LsfxCIvuy".to_owned()]),
        None,
        "letters after the `.` are the -e capability payload, not options",
    );
}

#[test]
fn refused_client_arg_matches_modify_window_short_form() {
    // `refuse options = modify-window` must match the short wire form `-@-1`,
    // which oc's OWN client emits (invocation/builder.rs:477, mirroring
    // options.c:2873-2875). `@` was missing from the short-letter map, so this
    // silently failed to refuse.
    let module = ModuleDefinition {
        refuse_options: vec!["modify-window".to_owned()],
        ..Default::default()
    };
    assert_eq!(
        refused_client_arg(&module, &["-@-1".to_owned()]),
        Some("--modify-window".to_owned())
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
    // upstream: options.c:989-1003 - rules are processed in order and the last
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
    // upstream: options.c:921-933 - `parse_one_refuse_match` matches each
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
    // upstream: options.c:959-980 - every option starts as `a*`/`a=`
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
fn refused_client_arg_copy_and_write_devices_refused_by_default() {
    // A daemon refuses `--copy-devices` and `--write-devices` even when the
    // module has no `refuse options` line at all: allowing a client to read or
    // write device nodes is a security-relevant default-deny.
    //
    // upstream: options.c:984-987 - when `am_daemon`, `parse_arguments` seeds
    // the refuse list with `parse_one_refuse_match(0, "copy-devices", ...)` and
    // `parse_one_refuse_match(0, "write-devices", ...)` before applying any
    // module rules.
    let module = ModuleDefinition {
        refuse_options: Vec::new(),
        ..Default::default()
    };
    assert_eq!(
        refused_client_arg(&module, &["--copy-devices".to_owned()]),
        Some("--copy-devices".to_owned())
    );
    assert_eq!(
        refused_client_arg(&module, &["--write-devices".to_owned()]),
        Some("--write-devices".to_owned())
    );
}

#[test]
fn refused_client_arg_non_device_option_not_refused_by_default() {
    // The default device refusal is scoped strictly to `copy-devices` and
    // `write-devices`; an empty refuse list must not refuse anything else.
    // upstream: options.c:984-987 - only those two options are seeded.
    let module = ModuleDefinition {
        refuse_options: Vec::new(),
        ..Default::default()
    };
    assert_eq!(
        refused_client_arg(&module, &["--compress".to_owned(), "-avzD".to_owned()]),
        None
    );
}

#[test]
fn refused_option_copy_and_write_devices_vital_survive_wildcard() {
    // `copy-devices`/`write-devices` are vital (exact-match only), so a blanket
    // `refuse options = *` neither un-refuses them nor is even consulted for
    // them - they remain refused by the daemon default.
    //
    // upstream: options.c:1050-1055 marks both as `descrip = "a="` (exact-match
    // only, wild-match disabled) and options.c:984-987 refuses them by default.
    let module = ModuleDefinition {
        refuse_options: vec!["*".to_owned()],
        ..Default::default()
    };
    assert_eq!(
        refused_option(&module, &["--copy-devices".to_owned()]),
        Some("--copy-devices")
    );
    assert_eq!(
        refused_option(&module, &["--write-devices".to_owned()]),
        Some("--write-devices")
    );
}

#[test]
fn refused_client_arg_device_options_allowed_by_explicit_negation() {
    // The default device refusal is overridable, but only by an explicit
    // negated exact match - never by a wildcard.
    //
    // upstream: options.c:984 - "Refused by default, but can be accepted via a
    // negated exact match." A module that trusts its clients can set
    // `refuse options = !copy-devices !write-devices`.
    let module = ModuleDefinition {
        refuse_options: vec!["!copy-devices".to_owned(), "!write-devices".to_owned()],
        ..Default::default()
    };
    assert_eq!(
        refused_client_arg(&module, &["--copy-devices".to_owned()]),
        None
    );
    assert_eq!(
        refused_client_arg(&module, &["--write-devices".to_owned()]),
        None
    );
}

#[test]
fn refused_client_arg_delete_rule_refuses_timing_variant() {
    // Regression for the upstream `daemon-refuse` testsuite: a module with
    // `refuse options = delete` must reject a client `-a --delete` even though
    // the client encodes it on the wire as `--delete-during`.
    //
    // upstream: options.c:2238 - `if (refused_delete && (delete_mode || ...))`
    // refuses the transfer whenever any delete-timing variant is active, and
    // reports the canonical `--delete` regardless of which variant arrived.
    let module = ModuleDefinition {
        refuse_options: vec!["delete".to_owned()],
        ..Default::default()
    };
    for variant in [
        "--delete",
        "--delete-during",
        "--delete-before",
        "--delete-after",
        "--delete-delay",
        "--delete-excluded",
        "--del",
        "--delete-missing-args",
    ] {
        let args = vec!["--server".to_owned(), variant.to_owned()];
        assert_eq!(
            refused_client_arg(&module, &args),
            Some("--delete".to_owned()),
            "expected {variant} to be refused as --delete",
        );
    }
}

#[test]
fn refused_client_arg_delete_rule_allows_non_delete_transfer() {
    // A `refuse options = delete` module must still accept a plain push that
    // carries no delete flag. upstream: options.c:2238 only fires when
    // `delete_mode` (or `missing_args == 2`) is set.
    let module = ModuleDefinition {
        refuse_options: vec!["delete".to_owned()],
        ..Default::default()
    };
    let args = vec!["--server".to_owned(), "-vlogDtpr".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_delete_negation_clears_semantic_refusal() {
    // `refuse options = !delete` un-refuses the `delete` entry, so the
    // semantic delete-mode pass must not fire. With no other refuse rule a
    // bare `--delete` transfer is allowed. upstream: options.c:989-1003 - the
    // negated rule flips the `delete` descrip back to accepted, clearing
    // `refused_delete`, so options.c:2238 never triggers.
    let module = ModuleDefinition {
        refuse_options: vec!["!delete".to_owned()],
        ..Default::default()
    };
    let args = vec!["--server".to_owned(), "--delete".to_owned()];
    assert_eq!(refused_client_arg(&module, &args), None);
}

#[test]
fn refused_client_arg_archive_rule_refuses_implied_short_letters() {
    // upstream: options.c:916-918 - the `archive` rule rewrites itself to the
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
    //
    // Upstream emits exactly the `@ERROR: <msg>\n` line and nothing more: the
    // client treats `@ERROR` as fatal and returns before reading further
    // (upstream: clientserver.c:424-428 - `strncmp(line, "@ERROR", 6) == 0` ->
    // `return -1`), so no trailing `@RSYNCD: EXIT` is sent. Matching that
    // byte-for-byte matters because two interoperating tools must agree on the
    // exact refusal wire bytes; a stray EXIT line is a divergence from upstream.
    let (mut stream, capture) = build_stdio_stream_with_capture();
    let mut limiter: Option<BandwidthLimiter> = None;
    send_error(
        &mut stream,
        &mut limiter,
        &AtError::message("The server is configured to refuse --delete"),
    )
    .expect("send pre-handshake error");

    let bytes = capture.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(
        text, "@ERROR: The server is configured to refuse --delete\n",
        "pre-handshake refusal must be exactly the raw @ERROR line with no trailing EXIT, got: {text:?}"
    );
    assert!(
        !text.contains("@RSYNCD: EXIT"),
        "upstream never follows @ERROR with @RSYNCD: EXIT, got: {text:?}"
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
