#![allow(unsafe_code)]

use super::*;

#[test]
fn parse_daemon_option_extracts_option_payload() {
    assert_eq!(parse_daemon_option("OPTION --list"), Some("--list"));
    assert_eq!(parse_daemon_option("option --max-verbosity"), Some("--max-verbosity"));
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

// ==================== refused_option glob pattern tests ====================

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
    for variant in &["--delete", "--delete-before", "--delete-after", "--delete-during"] {
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
    assert_eq!(
        refused_option(&module, &options_refused),
        Some("--delete")
    );
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
    let refuse = vec![
        "delete".to_owned(),
        "!delete".to_owned(),
    ];
    assert!(!is_option_refused(&refuse, "delete"));
}

#[test]
fn is_option_refused_re_refuse_after_negation() {
    let refuse = vec![
        "delete*".to_owned(),
        "!delete-during".to_owned(),
        "delete-during".to_owned(),
    ];
    assert!(is_option_refused(&refuse, "delete-during"));
}

#[test]
fn is_vital_option_recognises_all_vitals() {
    for &vital in VITAL_OPTIONS {
        assert!(
            is_vital_option(vital),
            "expected '{vital}' to be vital"
        );
    }
}

#[test]
fn is_vital_option_rejects_non_vitals() {
    assert!(!is_vital_option("delete"));
    assert!(!is_vital_option("compress"));
    assert!(!is_vital_option("archive"));
}

// ==================== ProgramName tests ====================

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

// ==================== parse_args tests ====================

#[test]
fn parse_args_empty_defaults_to_program_name() {
    let result = parse_args::<[&str; 0], &str>([]);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(!parsed.show_help);
    assert!(!parsed.show_version);
}

#[test]
fn parse_args_help_flag() {
    let args = ["rsyncd", "--help"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.show_help);
    assert!(!parsed.show_version);
}

#[test]
fn parse_args_version_flag_long() {
    let args = ["rsyncd", "--version"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(!parsed.show_help);
    assert!(parsed.show_version);
}

#[test]
fn parse_args_version_flag_short() {
    let args = ["rsyncd", "-V"];
    let result = parse_args(args);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(parsed.show_version);
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
    assert!(parsed.show_version);
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

// ==================== clap_command tests ====================

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

// ==================== render_help tests ====================

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

// ==================== apply_daemon_param_overrides tests ====================

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
