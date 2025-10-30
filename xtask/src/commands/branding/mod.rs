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
    use crate::workspace::parse_workspace_branding;

    #[test]
    fn execute_renders_branding_metadata() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manifest = tempdir.path().join("Cargo.toml");
        std::fs::write(&manifest, include_str!("../../../../Cargo.toml")).expect("write manifest");

        let workspace = tempdir.path();
        let branding = load_workspace_branding(workspace).expect("load branding");
        validate_branding(&branding).expect("validate branding");

        let output = render_branding(&branding, BrandingOutputFormat::Text).expect("render text");
        assert!(output.contains("Workspace branding summary:"));
    }

    #[test]
    fn manifest_branding_is_valid() {
        let manifest = include_str!("../../../../Cargo.toml");
        let branding = parse_workspace_branding(manifest).expect("workspace branding parses");
        validate_branding(&branding).expect("validation succeeds");
    }
}
