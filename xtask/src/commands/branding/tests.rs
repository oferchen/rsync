use super::*;
use crate::workspace::parse_workspace_branding;
use std::path::{Path, PathBuf};

fn sample_branding() -> WorkspaceBranding {
    WorkspaceBranding {
        brand: String::from("oc"),
        upstream_version: String::from("3.4.1"),
        rust_version: String::from("3.4.1-rust"),
        protocol: 32,
        client_bin: String::from("oc-rsync"),
        daemon_bin: String::from("oc-rsyncd"),
        legacy_client_bin: String::from("rsync"),
        legacy_daemon_bin: String::from("rsyncd"),
        daemon_config_dir: PathBuf::from("/etc/oc-rsyncd"),
        daemon_config: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.conf"),
        daemon_secrets: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.secrets"),
        legacy_daemon_config_dir: PathBuf::from("/etc"),
        legacy_daemon_config: PathBuf::from("/etc/rsyncd.conf"),
        legacy_daemon_secrets: PathBuf::from("/etc/rsyncd.secrets"),
        source: String::from("https://example.invalid/rsync"),
    }
}

fn manifest_branding() -> WorkspaceBranding {
    let manifest = include_str!("../../../../Cargo.toml");
    parse_workspace_branding(manifest).expect("manifest parses")
}

#[test]
fn parse_args_accepts_default_configuration() {
    let options = parse_args(std::iter::empty()).expect("parse succeeds");
    assert_eq!(options, BrandingOptions::default());
}

#[test]
fn parse_args_reports_help_request() {
    let error = parse_args([OsString::from("--help")]).unwrap_err();
    assert!(matches!(error, TaskError::Help(message) if message == usage()));
}

#[test]
fn parse_args_enables_json_output() {
    let options = parse_args([OsString::from("--json")]).expect("parse succeeds");
    assert_eq!(
        options,
        BrandingOptions {
            format: BrandingOutputFormat::Json,
        }
    );
}

#[test]
fn parse_args_rejects_duplicate_json_flags() {
    let error = parse_args([OsString::from("--json"), OsString::from("--json")]).unwrap_err();
    assert!(matches!(error, TaskError::Usage(message) if message.contains("--json")));
}

#[test]
fn parse_args_rejects_unknown_argument() {
    let error = parse_args([OsString::from("--unknown")]).unwrap_err();
    assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
}

#[test]
fn render_text_matches_expected_layout() {
    let branding = sample_branding();
    let rendered = render_branding_text(&branding);
    let expected = concat!(
        "Workspace branding summary:\n",
        "  brand: oc\n",
        "  upstream_version: 3.4.1\n",
        "  rust_version: 3.4.1-rust\n",
        "  protocol: 32\n",
        "  client_bin: oc-rsync\n",
        "  daemon_bin: oc-rsyncd\n",
        "  legacy_client_bin: rsync\n",
        "  legacy_daemon_bin: rsyncd\n",
        "  daemon_config_dir: /etc/oc-rsyncd\n",
        "  daemon_config: /etc/oc-rsyncd/oc-rsyncd.conf\n",
        "  daemon_secrets: /etc/oc-rsyncd/oc-rsyncd.secrets\n",
        "  legacy_daemon_config_dir: /etc\n",
        "  legacy_daemon_config: /etc/rsyncd.conf\n",
        "  legacy_daemon_secrets: /etc/rsyncd.secrets\n",
        "  source: https://example.invalid/rsync"
    );
    assert_eq!(rendered, expected);
}

#[test]
fn render_json_produces_expected_structure() {
    let branding = sample_branding();
    let rendered = render_branding_json(&branding).expect("json output");
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("parse json");
    assert_eq!(parsed["brand"], "oc");
    assert_eq!(parsed["protocol"], 32);
}

#[test]
fn render_branding_respects_selected_format() {
    let branding = sample_branding();
    let text = render_branding(&branding, BrandingOutputFormat::Text).expect("text");
    assert_eq!(text, render_branding_text(&branding));
    let json = render_branding(&branding, BrandingOutputFormat::Json).expect("json");
    let expected = render_branding_json(&branding).expect("json");
    assert_eq!(json, expected);
}

#[test]
fn validate_branding_accepts_manifest_configuration() {
    let branding = manifest_branding();
    validate_branding(&branding).expect("validation succeeds");
}

#[test]
fn validate_branding_accepts_prefixed_binaries() {
    let branding = sample_branding();
    validate_branding(&branding).expect("validation succeeds");
}

#[test]
fn validate_branding_rejects_protocol_outside_supported_range() {
    let mut branding = sample_branding();
    branding.protocol = 40;
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message) if message.contains("protocol version")
    ));
}

#[test]
fn validate_branding_rejects_missing_client_prefix() {
    let mut branding = sample_branding();
    branding.client_bin = String::from("rsync");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(error, TaskError::Validation(message) if message.contains("client binary")));
}

#[test]
fn validate_branding_rejects_missing_daemon_prefix() {
    let mut branding = sample_branding();
    branding.daemon_bin = String::from("rsyncd");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(error, TaskError::Validation(message) if message.contains("daemon binary")));
}

#[test]
fn validate_branding_rejects_empty_brand() {
    let mut branding = sample_branding();
    branding.brand.clear();
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(error, TaskError::Validation(message) if message.contains("brand label")));
}

#[test]
fn validate_branding_rejects_non_absolute_paths() {
    let mut branding = sample_branding();
    branding.daemon_config_dir = Path::new("relative").into();
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message) if message.contains("daemon_config_dir")
    ));
}

#[test]
fn validate_branding_rejects_empty_legacy_names() {
    let mut branding = sample_branding();
    branding.legacy_client_bin.clear();
    let client_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        client_error,
        TaskError::Validation(message) if message.contains("legacy client binary")
    ));

    let mut branding = sample_branding();
    branding.legacy_daemon_bin.clear();
    let daemon_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        daemon_error,
        TaskError::Validation(message) if message.contains("legacy daemon binary")
    ));
}

#[test]
fn validate_branding_rejects_incorrect_legacy_names() {
    let mut branding = sample_branding();
    branding.legacy_client_bin = String::from("legacy-rsync");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message) if message.contains("legacy client binary")
    ));

    let mut branding = sample_branding();
    branding.legacy_daemon_bin = String::from("legacy-rsyncd");
    let daemon_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        daemon_error,
        TaskError::Validation(message) if message.contains("legacy daemon binary")
    ));
}

#[test]
fn validate_branding_requires_legacy_directory() {
    let mut branding = sample_branding();
    branding.legacy_daemon_config_dir = PathBuf::from("/opt");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message) if message.contains("legacy_daemon_config_dir")
    ));
}

#[test]
fn validate_branding_requires_rust_version_prefix_and_suffix() {
    let mut branding = sample_branding();
    branding.rust_version = String::from("4.0.0-rust");
    let prefix_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        prefix_error,
        TaskError::Validation(message) if message.contains("must include upstream_version")
    ));

    let mut branding = sample_branding();
    branding.rust_version = String::from("3.4.1-custom");
    let suffix_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        suffix_error,
        TaskError::Validation(message) if message.contains("must end with")
    ));
}

#[test]
fn validate_branding_requires_paths_to_match_directories() {
    let mut branding = sample_branding();
    branding.daemon_config = PathBuf::from("/etc/oc-rsyncd.conf");
    let daemon_config_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        daemon_config_error,
        TaskError::Validation(message) if message.contains("daemon_config")
    ));

    let mut branding = sample_branding();
    branding.legacy_daemon_secrets = PathBuf::from("/var/lib/rsyncd.secrets");
    let legacy_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        legacy_error,
        TaskError::Validation(message) if message.contains("legacy_daemon_secrets")
    ));
}

#[test]
fn validate_branding_requires_daemon_directory_suffix() {
    let mut branding = sample_branding();
    branding.daemon_config_dir = PathBuf::from("/etc/oc-rsync");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message) if message.contains("daemon_config_dir")
    ));
}

#[test]
fn validate_branding_requires_daemon_file_names() {
    let mut branding = sample_branding();
    branding.daemon_config = PathBuf::from("/etc/oc-rsyncd/custom.conf");
    let config_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        config_error,
        TaskError::Validation(message)
            if message.contains("daemon_config '/etc/oc-rsyncd/custom.conf' must be named")
    ));

    let mut branding = sample_branding();
    branding.daemon_secrets = PathBuf::from("/etc/oc-rsyncd/rsyncd.secret");
    let secrets_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        secrets_error,
        TaskError::Validation(message) if message.contains("daemon_secrets")
    ));
}

#[test]
fn validate_branding_requires_legacy_file_names() {
    let mut branding = sample_branding();
    branding.legacy_daemon_config = PathBuf::from("/etc/rsync.conf");
    let config_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        config_error,
        TaskError::Validation(message) if message.contains("legacy_daemon_config")
    ));

    let mut branding = sample_branding();
    branding.legacy_daemon_secrets = PathBuf::from("/etc/rsync.secret");
    let secrets_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        secrets_error,
        TaskError::Validation(message) if message.contains("legacy_daemon_secrets")
    ));
}
