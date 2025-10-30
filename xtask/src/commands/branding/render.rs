use crate::error::{TaskError, TaskResult};
use crate::workspace::WorkspaceBranding;
use serde_json::json;

use super::args::BrandingOutputFormat;

pub(super) fn render_branding(
    branding: &WorkspaceBranding,
    format: BrandingOutputFormat,
) -> TaskResult<String> {
    match format {
        BrandingOutputFormat::Text => Ok(render_branding_text(branding)),
        BrandingOutputFormat::Json => render_branding_json(branding),
    }
}

pub(super) fn render_branding_text(branding: &WorkspaceBranding) -> String {
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

pub(super) fn render_branding_json(branding: &WorkspaceBranding) -> TaskResult<String> {
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
