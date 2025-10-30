use crate::error::{TaskError, TaskResult};
use crate::util::is_help_flag;
use crate::workspace::{WorkspaceBranding, load_workspace_branding};
use serde_json::json;
use std::ffi::OsString;
use std::path::Path;

/// Output format supported by the `branding` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BrandingOutputFormat {
    /// Human-readable text report.
    #[default]
    Text,
    /// Structured JSON report suitable for automation.
    Json,
}

/// Options accepted by the `branding` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrandingOptions {
    /// Desired output format.
    pub format: BrandingOutputFormat,
}

/// Parses CLI arguments for the `branding` command.
pub fn parse_args<I>(args: I) -> TaskResult<BrandingOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = BrandingOptions::default();

    for arg in args {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        let Some(raw) = arg.to_str() else {
            return Err(TaskError::Usage(String::from(
                "branding command arguments must be valid UTF-8",
            )));
        };

        match raw {
            "--json" => {
                if !matches!(options.format, BrandingOutputFormat::Text) {
                    return Err(TaskError::Usage(String::from(
                        "--json specified multiple times",
                    )));
                }
                options.format = BrandingOutputFormat::Json;
            }
            _ => {
                return Err(TaskError::Usage(format!(
                    "unrecognised argument '{raw}' for branding command",
                )));
            }
        }
    }

    Ok(options)
}

/// Executes the `branding` command.
pub fn execute(workspace: &Path, options: BrandingOptions) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    validate_branding(&branding)?;
    let output = render_branding(&branding, options.format)?;
    println!("{output}");
    Ok(())
}

fn validate_branding(branding: &WorkspaceBranding) -> TaskResult<()> {
    if branding.brand.trim().is_empty() {
        return Err(TaskError::Validation(String::from(
            "workspace brand label must not be empty",
        )));
    }

    let expected_prefix = format!("{}-", branding.brand);

    if !(28..=32).contains(&branding.protocol) {
        return Err(TaskError::Validation(format!(
            "protocol version {} must be between 28 and 32",
            branding.protocol
        )));
    }

    if !branding.client_bin.starts_with(&expected_prefix) {
        return Err(TaskError::Validation(format!(
            "client binary '{}' must use '{}' prefix",
            branding.client_bin, expected_prefix
        )));
    }

    if !branding.daemon_bin.starts_with(&expected_prefix) {
        return Err(TaskError::Validation(format!(
            "daemon binary '{}' must use '{}' prefix",
            branding.daemon_bin, expected_prefix
        )));
    }

    let expected_dir_suffix = format!("{}-rsyncd", branding.brand);
    if branding
        .daemon_config_dir
        .as_path()
        .file_name()
        .and_then(|name| name.to_str())
        != Some(expected_dir_suffix.as_str())
    {
        return Err(TaskError::Validation(format!(
            "daemon_config_dir '{}' must end with '{}'",
            branding.daemon_config_dir.display(),
            expected_dir_suffix
        )));
    }

    ensure_named_file(
        branding.daemon_config.as_path(),
        &format!("{}-rsyncd.conf", branding.brand),
        "daemon_config",
    )?;
    ensure_named_file(
        branding.daemon_secrets.as_path(),
        &format!("{}-rsyncd.secrets", branding.brand),
        "daemon_secrets",
    )?;
    ensure_named_file(
        branding.legacy_daemon_config.as_path(),
        "rsyncd.conf",
        "legacy_daemon_config",
    )?;
    ensure_named_file(
        branding.legacy_daemon_secrets.as_path(),
        "rsyncd.secrets",
        "legacy_daemon_secrets",
    )?;

    if branding.legacy_client_bin != "rsync" {
        return Err(TaskError::Validation(format!(
            "legacy client binary '{}' must be 'rsync'",
            branding.legacy_client_bin
        )));
    }

    if branding.legacy_daemon_bin != "rsyncd" {
        return Err(TaskError::Validation(format!(
            "legacy daemon binary '{}' must be 'rsyncd'",
            branding.legacy_daemon_bin
        )));
    }

    if branding.legacy_daemon_config_dir.as_path().to_str() != Some("/etc") {
        return Err(TaskError::Validation(format!(
            "legacy_daemon_config_dir '{}' must be '/etc'",
            branding.legacy_daemon_config_dir.display()
        )));
    }

    ensure_absolute(&branding.daemon_config_dir, "daemon_config_dir")?;
    ensure_absolute(&branding.daemon_config, "daemon_config")?;
    ensure_absolute(&branding.daemon_secrets, "daemon_secrets")?;
    ensure_absolute(
        &branding.legacy_daemon_config_dir,
        "legacy_daemon_config_dir",
    )?;
    ensure_absolute(&branding.legacy_daemon_config, "legacy_daemon_config")?;
    ensure_absolute(&branding.legacy_daemon_secrets, "legacy_daemon_secrets")?;

    if !branding
        .rust_version
        .starts_with(&branding.upstream_version)
    {
        return Err(TaskError::Validation(format!(
            "rust_version '{}' must include upstream_version '{}' prefix",
            branding.rust_version, branding.upstream_version
        )));
    }

    if !branding.rust_version.ends_with("-rust") {
        return Err(TaskError::Validation(format!(
            "rust_version '{}' must end with '-rust' suffix",
            branding.rust_version
        )));
    }

    ensure_under_dir(
        &branding.daemon_config,
        &branding.daemon_config_dir,
        "daemon_config",
        "daemon_config_dir",
    )?;
    ensure_under_dir(
        &branding.daemon_secrets,
        &branding.daemon_config_dir,
        "daemon_secrets",
        "daemon_config_dir",
    )?;
    ensure_under_dir(
        &branding.legacy_daemon_config,
        &branding.legacy_daemon_config_dir,
        "legacy_daemon_config",
        "legacy_daemon_config_dir",
    )?;
    ensure_under_dir(
        &branding.legacy_daemon_secrets,
        &branding.legacy_daemon_config_dir,
        "legacy_daemon_secrets",
        "legacy_daemon_config_dir",
    )?;

    Ok(())
}

fn ensure_absolute(path: &Path, label: &str) -> TaskResult<()> {
    if !path.is_absolute() {
        return Err(TaskError::Validation(format!(
            "{label} '{}' must be an absolute path",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_under_dir(
    path: &Path,
    expected_dir: &Path,
    path_label: &str,
    dir_label: &str,
) -> TaskResult<()> {
    let parent = path.parent().ok_or_else(|| {
        TaskError::Validation(format!(
            "{path_label} '{}' must reside under {dir_label} '{}'",
            path.display(),
            expected_dir.display()
        ))
    })?;

    if parent != expected_dir {
        return Err(TaskError::Validation(format!(
            "{path_label} '{}' must reside under {dir_label} '{}'",
            path.display(),
            expected_dir.display()
        )));
    }

    Ok(())
}

fn ensure_named_file(path: &Path, expected: &str, label: &str) -> TaskResult<()> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            TaskError::Validation(format!(
                "{label} '{}' must include a file name",
                path.display()
            ))
        })?;

    if name != expected {
        return Err(TaskError::Validation(format!(
            "{label} '{}' must be named '{expected}'",
            path.display()
        )));
    }

    Ok(())
}

fn render_branding(
    branding: &WorkspaceBranding,
    format: BrandingOutputFormat,
) -> TaskResult<String> {
    match format {
        BrandingOutputFormat::Text => Ok(render_branding_text(branding)),
        BrandingOutputFormat::Json => render_branding_json(branding),
    }
}

fn render_branding_text(branding: &WorkspaceBranding) -> String {
    format!(
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
            "  source: {}"
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
    )
}

fn render_branding_json(branding: &WorkspaceBranding) -> TaskResult<String> {
    let value = json!({
        "brand": branding.brand,
        "upstream_version": branding.upstream_version,
        "rust_version": branding.rust_version,
        "protocol": branding.protocol,
        "client_bin": branding.client_bin,
        "daemon_bin": branding.daemon_bin,
        "legacy_client_bin": branding.legacy_client_bin,
        "legacy_daemon_bin": branding.legacy_daemon_bin,
        "daemon_config_dir": branding.daemon_config_dir.display().to_string(),
        "daemon_config": branding.daemon_config.display().to_string(),
        "daemon_secrets": branding.daemon_secrets.display().to_string(),
        "legacy_daemon_config_dir": branding
            .legacy_daemon_config_dir
            .display()
            .to_string(),
        "legacy_daemon_config": branding
            .legacy_daemon_config
            .display()
            .to_string(),
        "legacy_daemon_secrets": branding
            .legacy_daemon_secrets
            .display()
            .to_string(),
        "source": branding.source,
    });

    serde_json::to_string_pretty(&value).map_err(|error| {
        TaskError::Metadata(format!(
            "failed to serialise branding metadata as JSON: {error}"
        ))
    })
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask branding [--json]\n\nOptions:\n  --json          Emit branding metadata in JSON format\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests {
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
        let manifest = include_str!("../../../Cargo.toml");
        parse_workspace_branding(manifest).expect("manifest parses")
    }

    fn expect_validation_error(mutate: impl FnOnce(&mut WorkspaceBranding), needle: &str) {
        let mut branding = sample_branding();
        mutate(&mut branding);
        match validate_branding(&branding) {
            Err(TaskError::Validation(message)) => assert!(
                message.contains(needle),
                "expected '{needle}' in '{message}'",
            ),
            Err(other) => panic!("expected validation error, got {other:?}"),
            Ok(_) => panic!("expected validation error"),
        }
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
        expect_validation_error(|branding| branding.protocol = 40, "protocol version");
    }

    #[test]
    fn validate_branding_rejects_missing_client_prefix() {
        expect_validation_error(
            |branding| branding.client_bin = String::from("rsync"),
            "client binary",
        );
    }

    #[test]
    fn validate_branding_rejects_missing_daemon_prefix() {
        expect_validation_error(
            |branding| branding.daemon_bin = String::from("rsyncd"),
            "daemon binary",
        );
    }

    #[test]
    fn validate_branding_rejects_empty_brand() {
        expect_validation_error(|branding| branding.brand.clear(), "brand label");
    }

    #[test]
    fn validate_branding_rejects_non_absolute_paths() {
        expect_validation_error(
            |branding| branding.daemon_config_dir = Path::new("relative").into(),
            "daemon_config_dir",
        );
    }

    #[test]
    fn validate_branding_rejects_empty_legacy_names() {
        expect_validation_error(
            |branding| branding.legacy_client_bin.clear(),
            "legacy client binary",
        );
        expect_validation_error(
            |branding| branding.legacy_daemon_bin.clear(),
            "legacy daemon binary",
        );
    }

    #[test]
    fn validate_branding_rejects_incorrect_legacy_names() {
        expect_validation_error(
            |branding| branding.legacy_client_bin = String::from("legacy-rsync"),
            "legacy client binary",
        );
        expect_validation_error(
            |branding| branding.legacy_daemon_bin = String::from("legacy-rsyncd"),
            "legacy daemon binary",
        );
    }

    #[test]
    fn validate_branding_requires_legacy_directory() {
        expect_validation_error(
            |branding| branding.legacy_daemon_config_dir = PathBuf::from("/opt"),
            "legacy_daemon_config_dir",
        );
    }

    #[test]
    fn validate_branding_requires_rust_version_prefix_and_suffix() {
        expect_validation_error(
            |branding| branding.rust_version = String::from("4.0.0-rust"),
            "must include upstream_version",
        );
        expect_validation_error(
            |branding| branding.rust_version = String::from("3.4.1-custom"),
            "must end with",
        );
    }

    #[test]
    fn validate_branding_requires_paths_to_match_directories() {
        expect_validation_error(
            |branding| branding.daemon_config = PathBuf::from("/etc/oc-rsyncd.conf"),
            "daemon_config",
        );
        expect_validation_error(
            |branding| branding.legacy_daemon_secrets = PathBuf::from("/var/lib/rsyncd.secrets"),
            "legacy_daemon_secrets",
        );
    }

    #[test]
    fn validate_branding_requires_daemon_directory_suffix() {
        expect_validation_error(
            |branding| branding.daemon_config_dir = PathBuf::from("/etc/oc-rsync"),
            "daemon_config_dir",
        );
    }

    #[test]
    fn validate_branding_requires_daemon_file_names() {
        expect_validation_error(
            |branding| branding.daemon_config = PathBuf::from("/etc/oc-rsyncd/custom.conf"),
            "daemon_config",
        );
        expect_validation_error(
            |branding| branding.daemon_secrets = PathBuf::from("/etc/oc-rsyncd/rsyncd.secret"),
            "daemon_secrets",
        );
    }

    #[test]
    fn validate_branding_requires_legacy_file_names() {
        expect_validation_error(
            |branding| branding.legacy_daemon_config = PathBuf::from("/etc/rsync.conf"),
            "legacy_daemon_config",
        );
        expect_validation_error(
            |branding| branding.legacy_daemon_secrets = PathBuf::from("/etc/rsync.secret"),
            "legacy_daemon_secrets",
        );
    }
}
