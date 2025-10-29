use super::PreflightOptions;
use crate::error::TaskResult;
use crate::util::cargo_metadata_json;
use crate::workspace::{parse_workspace_branding_from_value, read_workspace_manifest};
use std::path::Path;
use toml::Value;

use super::{
    branding::validate_branding, documentation::validate_documentation,
    packaging::validate_packaging, toolchain::validate_workspace_package_rust_version,
    versions::validate_package_versions,
};

/// Executes the `preflight` command.
pub fn execute(workspace: &Path, _options: PreflightOptions) -> TaskResult<()> {
    let manifest_text = read_workspace_manifest(workspace)?;
    let manifest_value = manifest_text.parse::<Value>().map_err(|error| {
        crate::error::TaskError::Metadata(format!("failed to parse workspace manifest: {error}"))
    })?;
    let branding = parse_workspace_branding_from_value(&manifest_value)?;

    validate_branding(&branding)?;
    validate_packaging(workspace, &branding)?;

    let metadata = cargo_metadata_json(workspace)?;
    validate_package_versions(&metadata, &branding)?;

    validate_workspace_package_rust_version(&manifest_value)?;
    validate_documentation(workspace, &branding)?;

    println!(
        "Preflight checks passed: branding, version, packaging metadata, documentation, and toolchain requirements validated."
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_runs_all_checks_with_default_options() {
        // Smoke-test the orchestration by ensuring the command executes against the
        // repository workspace. Individual validation logic is covered by unit tests
        // in their respective modules.
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(1)
            .unwrap();
        let options = PreflightOptions;
        let result = execute(workspace, options);

        // The workspace is expected to pass preflight checks.
        assert!(result.is_ok());
    }
}
