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

pub use args::{BrandingOptions, BrandingOutputFormat, parse_args};
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

    fn sample_manifest() -> String {
        String::from(
            r#"[workspace]
members = []

[workspace.metadata.oc_rsync]
brand = "demo"
upstream_version = "3.4.1"
rust_version = "3.4.1-rust"
protocol = 32
client_bin = "demo-rsync"
daemon_bin = "demo-rsyncd"
legacy_client_bin = "rsync"
legacy_daemon_bin = "rsyncd"
daemon_config_dir = "/etc/demo-rsyncd"
daemon_config = "/etc/demo-rsyncd/demo-rsyncd.conf"
daemon_secrets = "/etc/demo-rsyncd/demo-rsyncd.secrets"
legacy_daemon_config_dir = "/etc"
legacy_daemon_config = "/etc/rsyncd.conf"
legacy_daemon_secrets = "/etc/rsyncd.secrets"
source = "https://example.invalid/demo"
[workspace.metadata.oc_rsync.cross_compile]
linux = ["x86_64", "aarch64"]
macos = ["x86_64", "aarch64"]
windows = ["x86_64", "aarch64"]
"#,
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
                assert!(
                    inner
                        .to_string()
                        .contains("failed to read workspace manifest")
                );
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }
}
