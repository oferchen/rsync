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
            "  daemon_wrapper_bin: {}\n",
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
        branding.daemon_wrapper_bin,
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
        "daemon_wrapper_bin": branding.daemon_wrapper_bin,
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
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn sample_branding() -> WorkspaceBranding {
        WorkspaceBranding {
            brand: String::from("oc"),
            upstream_version: String::from("3.4.1"),
            rust_version: String::from("3.4.1-rust"),
            protocol: 32,
            client_bin: String::from("oc-rsync"),
            daemon_bin: String::from("oc-rsync"),
            daemon_wrapper_bin: String::from("oc-rsync"),
            legacy_client_bin: String::from("rsync"),
            legacy_daemon_bin: String::from("rsyncd"),
            daemon_config_dir: PathBuf::from("/etc/oc-rsyncd"),
            daemon_config: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.conf"),
            daemon_secrets: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.secrets"),
            legacy_daemon_config_dir: PathBuf::from("/etc"),
            legacy_daemon_config: PathBuf::from("/etc/rsyncd.conf"),
            legacy_daemon_secrets: PathBuf::from("/etc/rsyncd.secrets"),
            source: String::from("https://example.invalid/rsync"),
            cross_compile: BTreeMap::from([
                (
                    String::from("linux"),
                    vec![String::from("x86_64"), String::from("aarch64")],
                ),
                (
                    String::from("macos"),
                    vec![String::from("x86_64"), String::from("aarch64")],
                ),
                (String::from("windows"), vec![String::from("x86_64")]),
            ]),
            cross_compile_matrix: BTreeMap::from([
                (String::from("linux-x86_64"), true),
                (String::from("linux-aarch64"), true),
                (String::from("darwin-x86_64"), true),
                (String::from("darwin-aarch64"), true),
                (String::from("windows-x86_64"), true),
            ]),
        }
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
            "  daemon_bin: oc-rsync\n",
            "  daemon_wrapper_bin: oc-rsync\n",
            "  legacy_client_bin: rsync\n",
            "  legacy_daemon_bin: rsyncd\n",
            "  daemon_config_dir: /etc/oc-rsyncd\n",
            "  daemon_config: /etc/oc-rsyncd/oc-rsyncd.conf\n",
            "  daemon_secrets: /etc/oc-rsyncd/oc-rsyncd.secrets\n",
            "  legacy_daemon_config_dir: /etc\n",
            "  legacy_daemon_config: /etc/rsyncd.conf\n",
            "  legacy_daemon_secrets: /etc/rsyncd.secrets\n",
            "  source: https://example.invalid/rsync\n",
            "  cross_compile: Linux: x86_64, aarch64; macOS: x86_64, aarch64; Windows: x86_64"
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
