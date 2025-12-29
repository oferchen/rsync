//! Branding metadata inspection command.
//!
//! This module exposes the `cargo xtask branding` command which validates the
//! workspace branding configuration and renders it either as human-readable
//! text or JSON. The implementation is split across dedicated modules so the
//! parsing, validation, and rendering logic remain focused and individually
//! testable.

mod args;
mod render;
mod validate;

pub use args::{BrandingOptions, BrandingOutputFormat};
pub use render::render_branding;
pub use validate::validate_branding;

use crate::error::TaskResult;
use crate::workspace::load_workspace_branding;
use std::path::Path;

/// Executes the `branding` command.
pub fn execute(workspace: &Path, options: BrandingOptions) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    validate_branding(&branding)?;
    let output = render_branding(&branding, options.format)?;
    println!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TaskError;
    use crate::workspace::parse_workspace_branding;
    use branding::workspace;

    fn sample_manifest() -> String {
        let metadata = workspace::metadata();
        format!(
            r#"[workspace]
members = []

[workspace.metadata.oc_rsync]
brand = "{brand}"
upstream_version = "{upstream_version}"
rust_version = "{rust_version}"
protocol = {protocol}
client_bin = "{client_bin}"
daemon_bin = "{daemon_bin}"
legacy_client_bin = "{legacy_client_bin}"
legacy_daemon_bin = "{legacy_daemon_bin}"
daemon_config_dir = "{daemon_config_dir}"
daemon_config = "{daemon_config}"
daemon_secrets = "{daemon_secrets}"
legacy_daemon_config_dir = "{legacy_daemon_config_dir}"
legacy_daemon_config = "{legacy_daemon_config}"
legacy_daemon_secrets = "{legacy_daemon_secrets}"
source = "{source}"
[workspace.metadata.oc_rsync.cross_compile]
linux = ["x86_64", "aarch64"]
macos = ["x86_64", "aarch64"]
windows = ["x86_64", "aarch64"]
"#,
            brand = metadata.brand(),
            upstream_version = metadata.upstream_version(),
            rust_version = metadata.rust_version(),
            protocol = metadata.protocol_version(),
            client_bin = metadata.client_program_name(),
            daemon_bin = metadata.daemon_program_name(),
            legacy_client_bin = metadata.legacy_client_program_name(),
            legacy_daemon_bin = metadata.legacy_daemon_program_name(),
            daemon_config_dir = metadata.daemon_config_dir(),
            daemon_config = metadata.daemon_config_path(),
            daemon_secrets = metadata.daemon_secrets_path(),
            legacy_daemon_config_dir = metadata.legacy_daemon_config_dir(),
            legacy_daemon_config = metadata.legacy_daemon_config_path(),
            legacy_daemon_secrets = metadata.legacy_daemon_secrets_path(),
            source = metadata.source_url(),
        )
    }

    #[test]
    fn execute_renders_branding_metadata() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manifest_path = tempdir.path().join("Cargo.toml");
        std::fs::write(&manifest_path, sample_manifest()).expect("write manifest");

        execute(
            tempdir.path(),
            BrandingOptions {
                format: BrandingOutputFormat::Json,
            },
        )
        .expect("execute succeeds");
    }

    #[test]
    fn manifest_branding_is_valid() {
        let manifest = include_str!("../../../../Cargo.toml");
        let branding = parse_workspace_branding(manifest).expect("workspace branding parses");
        validate_branding(&branding).expect("validation succeeds");
    }

    #[test]
    fn execute_reports_missing_manifest() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let error = execute(tempdir.path(), BrandingOptions::default()).unwrap_err();
        match error {
            TaskError::Io(inner) => {
                let message = inner.to_string();
                assert!(
                    message.contains("failed to read") && message.contains("Cargo.toml"),
                    "expected error about reading Cargo.toml, got: {message}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
