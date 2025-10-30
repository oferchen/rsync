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
mod tests;
