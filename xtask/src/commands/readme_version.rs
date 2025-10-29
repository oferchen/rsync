#![deny(unsafe_code)]

//! README version consistency check command.
//!
//! The `readme-version` command validates that the workspace `README.md`
//! mentions both the upstream rsync release targeted by this build and the
//! Rust-branded version string advertised by the shipping binaries. Keeping the
//! documentation in lock-step with the binaries avoids stale marketing
//! materials and ensures downstream packagers can rely on the README as the
//! canonical human-readable reference for the supported release line.

use crate::error::{TaskError, TaskResult};
use crate::util::is_help_flag;
use crate::workspace::load_workspace_branding;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

/// Options accepted by the `readme-version` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadmeVersionOptions;

/// Parses CLI arguments for the `readme-version` command.
pub fn parse_args<I>(args: I) -> TaskResult<ReadmeVersionOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for readme-version command",
            arg.to_string_lossy()
        )));
    }

    Ok(ReadmeVersionOptions)
}

/// Executes the `readme-version` command.
pub fn execute(workspace: &Path, _options: ReadmeVersionOptions) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    let readme_path = workspace.join("README.md");
    let readme = readme_contents(&readme_path)?;

    validate_contains(&readme, &branding.rust_version, "Rust-branded version")?;
    validate_contains(&readme, &branding.upstream_version, "upstream version")?;

    println!(
        "README.md references versions '{}' and '{}'",
        branding.rust_version, branding.upstream_version
    );

    Ok(())
}

fn readme_contents(path: &Path) -> TaskResult<String> {
    fs::read_to_string(path).map_err(|error| {
        TaskError::Io(std::io::Error::new(
            error.kind(),
            format!("failed to read README at {}: {error}", path.display()),
        ))
    })
}

fn validate_contains(readme: &str, needle: &str, label: &str) -> TaskResult<()> {
    if readme.contains(needle) {
        Ok(())
    } else {
        Err(TaskError::Validation(format!(
            "README.md is missing {label} '{needle}'"
        )))
    }
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask readme-version\n\nOptions:\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, ReadmeVersionOptions);
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("readme-version")));
    }

    #[test]
    fn validate_contains_accepts_present_version() {
        validate_contains("rsync 3.4.1-rust", "3.4.1-rust", "rust version")
            .expect("validation succeeds");
    }

    #[test]
    fn validate_contains_rejects_missing_version() {
        let error = validate_contains("rsync", "3.4.1", "upstream version").unwrap_err();
        assert!(matches!(error, TaskError::Validation(message) if message.contains("README.md")));
    }

    #[test]
    fn execute_reports_versions_present() {
        let workspace = workspace::workspace_root().expect("resolve workspace root");
        execute(&workspace, ReadmeVersionOptions).expect("workspace README matches versions");
    }

    mod workspace {
        use super::*;

        pub fn workspace_root() -> TaskResult<PathBuf> {
            crate::workspace::workspace_root()
        }
    }
}
