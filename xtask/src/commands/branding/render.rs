use super::BrandingOutputFormat;
use crate::error::{TaskError, TaskResult};
use crate::workspace::WorkspaceBranding;
use serde_json::json;
use std::collections::BTreeMap;

pub fn render_branding(
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
        format_cross_compile_summary(&branding.cross_compile),
    )
}

fn render_branding_json(branding: &WorkspaceBranding) -> TaskResult<String> {
    let cross_compile: BTreeMap<_, _> = branding
        .cross_compile
        .iter()
        .map(|(os, archs)| (os.clone(), archs.clone()))
        .collect();

    let cross_compile_matrix: BTreeMap<_, _> = branding
        .cross_compile_matrix
        .iter()
        .map(|(platform, enabled)| (platform.clone(), *enabled))
        .collect();

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
        "cross_compile": cross_compile,
        "cross_compile_matrix": cross_compile_matrix,
    });

    serde_json::to_string_pretty(&value).map_err(|error| {
        TaskError::Metadata(format!(
            "failed to serialise branding metadata as JSON: {error}"
        ))
    })
}

fn format_cross_compile_summary(matrix: &BTreeMap<String, Vec<String>>) -> String {
    matrix
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
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    fn sample_branding() -> WorkspaceBranding {
        test_support::workspace_branding_snapshot()
    }

    #[test]
    fn render_text_matches_expected_layout() {
        let branding = sample_branding();
        let rendered = render_branding_text(&branding);
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
            format_cross_compile_summary(&branding.cross_compile)
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
}
