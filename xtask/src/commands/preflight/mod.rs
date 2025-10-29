mod branding;
mod documentation;
mod packaging;
mod versions;

use crate::error::{TaskError, TaskResult};
use crate::util::is_help_flag;
use crate::workspace::{parse_workspace_branding_from_value, read_workspace_manifest};
use std::ffi::OsString;
use std::path::Path;
use toml::Value;

use self::branding::validate_branding;
use self::documentation::validate_documentation;
use self::packaging::validate_packaging_assets;
use self::versions::{validate_package_versions, validate_workspace_package_rust_version};

/// Options accepted by the `preflight` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PreflightOptions;

/// Parses CLI arguments for the `preflight` command.
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
    let manifest_value = manifest_text.parse::<Value>().map_err(|error| {
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
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask preflight\n\nOptions:\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, PreflightOptions);
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("preflight")));
    }
}
