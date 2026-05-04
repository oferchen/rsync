use super::args::{BrandingOptions, BrandingOutputFormat, parse_args, usage};
use super::render::{render_branding, render_branding_json, render_branding_text};
use super::validate::validate_branding;
use crate::error::TaskError;
use crate::test_support;
use crate::workspace::{WorkspaceBranding, parse_workspace_branding};
use std::ffi::OsString;
use std::path::PathBuf;

fn sample_branding() -> WorkspaceBranding {
    test_support::workspace_branding_snapshot()
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
    let cross_compile_summary = branding
        .cross_compile
        .iter()
        .map(|(os, archs)| {
            let label = match os.as_str() {
                "linux" => "Linux",
                "macos" => "macOS",
                "windows" => "Windows",
                other => other,
            };
            if archs.is_empty() {
                format!("{label}: (none)")
            } else {
                format!("{label}: {}", archs.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ");

    let expected = format!(
        concat!(
            "Workspace branding summary:\n",
            "  brand: {}\n",
            "  upstream_version: {}\n",
            "  rust_version: {}\n",
            "  protocol: {}\n",
            "  client_bin: {}\n",
            "  daemon_bin: {}\n",
            "  legacy_client_bin: {}\n",
            "  legacy_daemon_bin: {}\n",
            "  daemon_config_dir: {}\n",
            "  daemon_config: {}\n",
            "  daemon_secrets: {}\n",
            "  legacy_daemon_config_dir: {}\n",
            "  legacy_daemon_config: {}\n",
            "  legacy_daemon_secrets: {}\n",
            "  source: {}\n",
            "  cross_compile: {}"
        ),
        branding.brand,
        branding.upstream_version,
        branding.rust_version,
        branding.protocol,
        branding.client_bin,
        branding.daemon_bin,
        branding.legacy_client_bin,
        branding.legacy_daemon_bin,
        branding.daemon_config_dir.display(),
        branding.daemon_config.display(),
        branding.daemon_secrets.display(),
        branding.legacy_daemon_config_dir.display(),
        branding.legacy_daemon_config.display(),
        branding.legacy_daemon_secrets.display(),
        branding.source,
        cross_compile_summary
    );
    assert_eq!(rendered, expected);
}

#[test]
fn render_json_produces_expected_structure() {
    let branding = sample_branding();
    let rendered = render_branding_json(&branding).expect("json output");
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("parse json");
    assert_eq!(parsed["brand"], branding.brand);
    assert_eq!(parsed["protocol"], branding.protocol);
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
fn validate_branding_rejects_client_whitespace() {
    let mut branding = sample_branding();
    branding.client_bin = String::from("oc rsync");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message)
            if message.contains("client_bin") && message.contains("whitespace")
    ));
}

#[test]
fn validate_branding_rejects_daemon_with_separator() {
    let mut branding = sample_branding();
    branding.daemon_bin = String::from("bin/oc-rsync");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message)
            if message.contains("daemon_bin") && message.contains("path separators")
    ));
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
    branding.daemon_config_dir = PathBuf::from("oc-rsyncd");
    let dir_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        dir_error,
        TaskError::Validation(message) if message.contains("must be an absolute path")
    ));

    let mut branding = sample_branding();
    branding.daemon_config = PathBuf::from("oc-rsyncd/oc-rsyncd.conf");
    let config_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        config_error,
        TaskError::Validation(message) if message.contains("must be an absolute path")
    ));

    let mut branding = sample_branding();
    branding.legacy_daemon_secrets = PathBuf::from("rsyncd.secrets");
    let legacy_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        legacy_error,
        TaskError::Validation(message) if message.contains("must be an absolute path")
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
fn validate_branding_rejects_relative_daemon_directory() {
    let mut branding = sample_branding();
    branding.daemon_config_dir = PathBuf::from("etc/oc-rsyncd");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message)
            if message.contains("daemon_config_dir") && message.contains("absolute path")
    ));
}

#[test]
fn validate_branding_requires_daemon_file_names() {
    let mut branding = sample_branding();
    branding.daemon_config = PathBuf::from("/etc/oc-rsyncd");
    let config_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        config_error,
        TaskError::Validation(message)
            if message.contains("daemon_config") && message.contains("must not be identical")
    ));

    let mut branding = sample_branding();
    branding.daemon_secrets = PathBuf::from("/etc/oc-rsyncd");
    let secrets_error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        secrets_error,
        TaskError::Validation(message)
            if message.contains("daemon_secrets") && message.contains("must not be identical")
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

#[test]
fn validate_branding_rejects_binary_with_path_separator() {
    let mut branding = sample_branding();
    branding.client_bin = String::from("bin/oc-rsync");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message)
            if message.contains("client_bin") && message.contains("path separators")
    ));
}

#[test]
fn validate_branding_rejects_binary_with_whitespace() {
    let mut branding = sample_branding();
    branding.daemon_bin = String::from("oc rsync");
    let error = validate_branding(&branding).unwrap_err();
    assert!(matches!(
        error,
        TaskError::Validation(message)
            if message.contains("daemon_bin") && message.contains("whitespace")
    ));
}

#[test]
fn execute_writes_expected_output() {
    let branding = sample_branding();
    let output = render_branding(&branding, BrandingOutputFormat::Text).expect("text");
    assert_eq!(output, render_branding_text(&branding));
}
