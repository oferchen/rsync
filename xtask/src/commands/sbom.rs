use crate::error::{TaskError, TaskResult};
use crate::util::{is_help_flag, run_cargo_tool};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// Options accepted by the `sbom` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SbomOptions {
    /// Optional override for the SBOM output path.
    pub output: Option<PathBuf>,
}

/// Parses CLI arguments for the `sbom` command.
pub fn parse_args<I>(args: I) -> TaskResult<SbomOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut output = None;

    while let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        if arg == "--output" {
            let value = args.next().ok_or_else(|| {
                TaskError::Usage(String::from(
                    "--output requires a path argument; see `cargo xtask sbom --help`",
                ))
            })?;

            if output.is_some() {
                return Err(TaskError::Usage(String::from(
                    "--output specified multiple times",
                )));
            }

            output = Some(PathBuf::from(value));
            continue;
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for sbom command",
            arg.to_string_lossy()
        )));
    }

    Ok(SbomOptions { output })
}

/// Executes the `sbom` command.
pub fn execute(workspace: &Path, options: SbomOptions) -> TaskResult<()> {
    let default_output = PathBuf::from("target/sbom/rsync.cdx.json");
    let raw_output = options.output.unwrap_or(default_output);
    let output_path = if raw_output.is_absolute() {
        raw_output
    } else {
        workspace.join(raw_output)
    };

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    println!("Generating SBOM at {}", output_path.display());

    let manifest_path = workspace.join("Cargo.toml");
    let args = vec![
        OsString::from("cyclonedx"),
        OsString::from("--manifest-path"),
        manifest_path.into_os_string(),
        OsString::from("--workspace"),
        OsString::from("--format"),
        OsString::from("json"),
        OsString::from("--output"),
        output_path.into_os_string(),
        OsString::from("--all-features"),
        OsString::from("--locked"),
    ];

    run_cargo_tool(
        workspace,
        args,
        "cargo cyclonedx",
        "install the cargo-cyclonedx subcommand (cargo install cargo-cyclonedx)",
    )
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask sbom [--output PATH]\n\nOptions:\n  --output PATH    Override the SBOM output path (relative to the workspace root unless absolute)\n  -h, --help       Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, SbomOptions { output: None });
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_accepts_output_override() {
        let options = parse_args([OsString::from("--output"), OsString::from("custom.json")])
            .expect("parse succeeds");
        assert_eq!(
            options,
            SbomOptions {
                output: Some(PathBuf::from("custom.json")),
            }
        );
    }

    #[test]
    fn parse_args_rejects_duplicate_output_flags() {
        let error = parse_args([
            OsString::from("--output"),
            OsString::from("one.json"),
            OsString::from("--output"),
        ])
        .unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--output")));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }
}
