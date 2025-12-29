use crate::error::{TaskError, TaskResult};
use crate::workspace::{parse_workspace_branding_from_value, read_workspace_manifest};
use std::path::Path;
use toml::Value;

mod validation;
use validation::{
    validate_branding, validate_documentation, validate_package_versions,
    validate_packaging_assets, validate_workspace_package_rust_version,
};

/// Options accepted by the `preflight` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PreflightOptions;

/// Executes the `preflight` command.
pub fn execute(workspace: &Path, _options: PreflightOptions) -> TaskResult<()> {
    let manifest_text = read_workspace_manifest(workspace)?;
    let manifest_value: Value = manifest_text.parse().map_err(|error| {
        TaskError::Metadata(format!("failed to parse workspace manifest: {error}"))
    })?;
    let branding = parse_workspace_branding_from_value(&manifest_value)?;

    validate_branding(&branding)?;
    validate_packaging_assets(workspace, &branding)?;
    validate_package_versions(workspace, &branding)?;
    validate_workspace_package_rust_version(&manifest_value)?;
    validate_documentation(workspace, &branding)?;

    println!(
        "Preflight checks passed: branding, version, packaging metadata, documentation, and toolchain requirements validated."
    );

    Ok(())
}
