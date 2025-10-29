use crate::commands::{
    docs::{self, DocsOptions},
    enforce_limits::{self, EnforceLimitsOptions},
    no_binaries::{self, NoBinariesOptions},
    no_placeholders::{self, NoPlaceholdersOptions},
    preflight::{self, PreflightOptions},
    readme_version::{self, ReadmeVersionOptions},
};
use crate::error::{TaskError, TaskResult};
use crate::util::is_help_flag;
use std::ffi::OsString;
use std::path::Path;

/// Options accepted by the `release` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReleaseOptions {
    /// Skip rebuilding API documentation and doctests.
    pub skip_docs: bool,
    /// Skip source line-count enforcement checks.
    pub skip_hygiene: bool,
    /// Skip placeholder scans for Rust sources.
    pub skip_placeholder_scan: bool,
    /// Skip auditing the git index for tracked binary artifacts.
    pub skip_binary_scan: bool,
}

/// Parses CLI arguments for the `release` command.
pub fn parse_args<I>(args: I) -> TaskResult<ReleaseOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = ReleaseOptions::default();

    for arg in args.into_iter() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        match arg.to_string_lossy().as_ref() {
            "--skip-docs" => {
                ensure_flag_unused(options.skip_docs, "--skip-docs")?;
                options.skip_docs = true;
            }
            "--skip-hygiene" => {
                ensure_flag_unused(options.skip_hygiene, "--skip-hygiene")?;
                options.skip_hygiene = true;
            }
            "--skip-placeholder-scan" => {
                ensure_flag_unused(options.skip_placeholder_scan, "--skip-placeholder-scan")?;
                options.skip_placeholder_scan = true;
            }
            "--skip-binary-scan" => {
                ensure_flag_unused(options.skip_binary_scan, "--skip-binary-scan")?;
                options.skip_binary_scan = true;
            }
            other => {
                return Err(TaskError::Usage(format!(
                    "unrecognised argument '{other}' for release command"
                )));
            }
        }
    }

    Ok(options)
}

fn ensure_flag_unused(already_set: bool, flag: &str) -> TaskResult<()> {
    if already_set {
        Err(TaskError::Usage(format!(
            "{flag} was specified multiple times"
        )))
    } else {
        Ok(())
    }
}

/// Executes the `release` command.
pub fn execute(workspace: &Path, options: ReleaseOptions) -> TaskResult<()> {
    let mut executed_steps = Vec::new();
    let mut skipped_steps = Vec::new();

    if options.skip_binary_scan {
        skipped_steps.push("no-binaries");
    } else {
        no_binaries::execute(workspace, NoBinariesOptions)?;
        executed_steps.push("no-binaries");
    }

    preflight::execute(workspace, PreflightOptions)?;
    executed_steps.push("preflight");

    readme_version::execute(workspace, ReadmeVersionOptions)?;
    executed_steps.push("readme-version");

    if options.skip_docs {
        skipped_steps.push("docs");
    } else {
        docs::execute(workspace, DocsOptions::default())?;
        executed_steps.push("docs");
    }

    if options.skip_hygiene {
        skipped_steps.push("enforce-limits");
    } else {
        enforce_limits::execute(workspace, EnforceLimitsOptions::default())?;
        executed_steps.push("enforce-limits");
    }

    if options.skip_placeholder_scan {
        skipped_steps.push("no-placeholders");
    } else {
        no_placeholders::execute(workspace, NoPlaceholdersOptions)?;
        executed_steps.push("no-placeholders");
    }

    if skipped_steps.is_empty() {
        println!(
            "Release validation complete. Executed steps: {}.",
            executed_steps.join(", ")
        );
    } else {
        println!(
            "Release validation complete. Executed: {}; skipped: {}.",
            executed_steps.join(", "),
            skipped_steps.join(", ")
        );
    }

    Ok(())
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask release [OPTIONS]\n\nOptions:\n  --skip-docs                Skip building docs and running doctests\n  --skip-hygiene            Skip enforce-limits line-count checks\n  --skip-placeholder-scan   Skip placeholder detection scans\n  --skip-binary-scan        Skip checking the git index for binary files\n  -h, --help                Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, ReleaseOptions::default());
    }

    #[test]
    fn parse_args_recognises_all_skip_flags() {
        let args = [
            OsString::from("--skip-docs"),
            OsString::from("--skip-hygiene"),
            OsString::from("--skip-placeholder-scan"),
            OsString::from("--skip-binary-scan"),
        ];
        let options = parse_args(args).expect("parse succeeds");
        assert!(options.skip_docs);
        assert!(options.skip_hygiene);
        assert!(options.skip_placeholder_scan);
        assert!(options.skip_binary_scan);
    }

    #[test]
    fn parse_args_rejects_duplicate_flags() {
        let args = [OsString::from("--skip-docs"), OsString::from("--skip-docs")];
        let error = parse_args(args).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("skip-docs")));
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn execute_runs_core_checks_when_optional_steps_skipped() {
        let workspace = workspace::workspace_root().expect("workspace root");
        let options = ReleaseOptions {
            skip_docs: true,
            skip_hygiene: true,
            skip_placeholder_scan: true,
            skip_binary_scan: true,
        };
        execute(&workspace, options).expect("release validation succeeds");
    }
}
