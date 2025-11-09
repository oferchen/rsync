use crate::workspace::{self, WorkspaceBranding};
use std::fmt::Write;

/// Returns a [`WorkspaceBranding`] snapshot loaded from the repository manifest.
#[must_use]
pub(crate) fn workspace_branding_snapshot() -> WorkspaceBranding {
    let workspace = workspace::workspace_root().expect("workspace root available");
    workspace::load_workspace_branding(&workspace).expect("load workspace branding")
}

/// Renders a minimal workspace manifest snippet mirroring the active branding configuration.
#[must_use]
pub(crate) fn manifest_snippet() -> String {
    manifest_snippet_from_branding(&workspace_branding_snapshot())
}

/// Renders a workspace manifest snippet for the provided branding configuration.
#[must_use]
pub(crate) fn manifest_snippet_from_branding(branding: &WorkspaceBranding) -> String {
    let mut snippet = String::new();
    writeln!(&mut snippet, "[workspace]").expect("write workspace header");
    writeln!(&mut snippet, "members = []").expect("write workspace members");
    snippet.push('\n');
    writeln!(&mut snippet, "[workspace.metadata]").expect("write metadata header");
    snippet.push('\n');
    writeln!(&mut snippet, "[workspace.metadata.oc_rsync]").expect("write branding header");
    push_string(&mut snippet, "brand", &branding.brand);
    push_string(&mut snippet, "upstream_version", &branding.upstream_version);
    push_string(&mut snippet, "rust_version", &branding.rust_version);
    writeln!(&mut snippet, "protocol = {}", branding.protocol).expect("write protocol");
    push_string(&mut snippet, "client_bin", &branding.client_bin);
    push_string(&mut snippet, "daemon_bin", &branding.daemon_bin);
    push_string(
        &mut snippet,
        "legacy_client_bin",
        &branding.legacy_client_bin,
    );
    push_string(
        &mut snippet,
        "legacy_daemon_bin",
        &branding.legacy_daemon_bin,
    );
    push_path(
        &mut snippet,
        "daemon_config_dir",
        &branding.daemon_config_dir,
    );
    push_path(&mut snippet, "daemon_config", &branding.daemon_config);
    push_path(&mut snippet, "daemon_secrets", &branding.daemon_secrets);
    push_path(
        &mut snippet,
        "legacy_daemon_config_dir",
        &branding.legacy_daemon_config_dir,
    );
    push_path(
        &mut snippet,
        "legacy_daemon_config",
        &branding.legacy_daemon_config,
    );
    push_path(
        &mut snippet,
        "legacy_daemon_secrets",
        &branding.legacy_daemon_secrets,
    );
    push_string(&mut snippet, "source", &branding.source);

    if !branding.cross_compile.is_empty() {
        snippet.push('\n');
        writeln!(&mut snippet, "[workspace.metadata.oc_rsync.cross_compile]")
            .expect("write cross compile header");
        for (os, targets) in &branding.cross_compile {
            writeln!(&mut snippet, "{os} = {}", format_array(targets))
                .expect("write cross compile entry");
        }
    }

    if !branding.cross_compile_matrix.is_empty() {
        snippet.push('\n');
        writeln!(
            &mut snippet,
            "[workspace.metadata.oc_rsync.cross_compile_matrix]"
        )
        .expect("write cross compile matrix header");
        for (platform, enabled) in &branding.cross_compile_matrix {
            writeln!(&mut snippet, "\"{platform}\" = {}", enabled).expect("write matrix entry");
        }
    }

    snippet
}

fn push_string(buffer: &mut String, key: &str, value: &str) {
    writeln!(buffer, "{key} = {}", toml::Value::String(value.to_owned()))
        .expect("write string entry");
}

fn push_path(buffer: &mut String, key: &str, value: &std::path::Path) {
    let rendered = value.display().to_string();
    push_string(buffer, key, &rendered);
}

fn format_array(values: &[String]) -> String {
    if values.is_empty() {
        return String::from("[]");
    }

    let quoted: Vec<String> = values
        .iter()
        .map(|value| toml::Value::String(value.clone()).to_string())
        .collect();
    format!("[{}]", quoted.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_snippet_round_trips_branding() {
        let branding = workspace_branding_snapshot();
        let snippet = manifest_snippet_from_branding(&branding);
        let parsed = crate::workspace::parse_workspace_branding(&snippet)
            .expect("parsed branding from snippet");
        assert_eq!(branding.brand, parsed.brand);
        assert_eq!(branding.client_bin, parsed.client_bin);
        assert_eq!(branding.daemon_bin, parsed.daemon_bin);
        assert_eq!(branding.source, parsed.source);
        assert_eq!(branding.protocol, parsed.protocol);
        assert_eq!(branding.cross_compile, parsed.cross_compile);
        assert_eq!(branding.cross_compile_matrix, parsed.cross_compile_matrix);
    }

    #[test]
    fn format_array_handles_empty_values() {
        assert_eq!(format_array(&[]), "[]");
        assert_eq!(format_array(&[String::from("x86_64")]), "[\"x86_64\"]");
        assert_eq!(
            format_array(&[String::from("x86_64"), String::from("aarch64")]),
            "[\"x86_64\", \"aarch64\"]"
        );
    }
}
