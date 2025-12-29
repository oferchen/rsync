use crate::error::{TaskError, TaskResult};
#[cfg(test)]
use crate::util::is_help_flag;
use crate::workspace::{parse_workspace_branding_from_value, read_workspace_manifest};
#[cfg(test)]
use std::ffi::OsString;
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

/// Parses CLI arguments for the `preflight` command.
#[cfg(test)]
pub fn parse_args<I>(args: I) -> TaskResult<PreflightOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for preflight command",
            arg.to_string_lossy()
        )));
    }

    Ok(PreflightOptions)
}

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

/// Returns usage text for the command.
#[cfg(test)]
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask preflight\n\nOptions:\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests;
