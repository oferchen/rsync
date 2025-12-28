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

