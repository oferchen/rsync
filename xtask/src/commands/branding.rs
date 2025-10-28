use crate::error::{TaskError, TaskResult};
use crate::util::is_help_flag;
use crate::workspace::{WorkspaceBranding, load_workspace_branding};
use serde_json::json;
use std::ffi::OsString;
use std::path::Path;

/// Output format supported by the `branding` command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrandingOutputFormat {
    /// Human-readable text report.
    Text,
    /// Structured JSON report suitable for automation.
    Json,
}

impl Default for BrandingOutputFormat {
    fn default() -> Self {
        BrandingOutputFormat::Text
    }
}

/// Options accepted by the `branding` command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrandingOptions {
    /// Desired output format.
    pub format: BrandingOutputFormat,
}

impl Default for BrandingOptions {
    fn default() -> Self {
        BrandingOptions {
            format: BrandingOutputFormat::default(),
        }
    }
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
    let output = render_branding(&branding, options.format)?;
    println!("{output}");
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
        branding.daemon_config_dir,
        branding.daemon_config,
        branding.daemon_secrets,
        branding.legacy_daemon_config_dir,
        branding.legacy_daemon_config,
        branding.legacy_daemon_secrets,
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
        "daemon_config_dir": branding.daemon_config_dir,
        "daemon_config": branding.daemon_config,
        "daemon_secrets": branding.daemon_secrets,
        "legacy_daemon_config_dir": branding.legacy_daemon_config_dir,
        "legacy_daemon_config": branding.legacy_daemon_config,
        "legacy_daemon_secrets": branding.legacy_daemon_secrets,
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
            daemon_config_dir: String::from("/etc/oc-rsyncd"),
            daemon_config: String::from("/etc/oc-rsyncd/oc-rsyncd.conf"),
            daemon_secrets: String::from("/etc/oc-rsyncd/oc-rsyncd.secrets"),
            legacy_daemon_config_dir: String::from("/etc"),
            legacy_daemon_config: String::from("/etc/rsyncd.conf"),
            legacy_daemon_secrets: String::from("/etc/rsyncd.secrets"),
            source: String::from("https://example.invalid/rsync"),
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
}
