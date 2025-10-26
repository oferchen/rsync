#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! # Overview
//!
//! The `xtask` utility hosts workspace maintenance commands that are not part of
//! the shipping binaries. The current implementation focuses on producing a
//! CycloneDX Software Bill of Materials (SBOM) so packaging automation can ship
//! reproducible metadata alongside the Rust `oc-rsync` binaries.
//!
//! Invocations follow the conventional `cargo xtask <command>` pattern. The
//! `sbom` command executes the installed `cargo-cyclonedx` plugin with the
//! workspace manifest and writes the resulting JSON document to
//! `target/sbom/rsync.cdx.json` unless an explicit output path is provided.
//!
//! # Examples
//!
//! Generate the default SBOM. The example is marked `no_run` because the host
//! environment must have the `cargo-cyclonedx` plugin installed for the command
//! to succeed.
//!
//! ```no_run
//! std::process::Command::new("cargo")
//!     .args(["run", "-p", "xtask", "--", "sbom"])
//!     .status()
//!     .expect("invoke xtask sbom");
//! ```
//!
//! Request a custom output location relative to the workspace root:
//!
//! ```no_run
//! std::process::Command::new("cargo")
//!     .args([
//!         "run",
//!         "-p",
//!         "xtask",
//!         "--",
//!         "sbom",
//!         "--output",
//!         "artifacts/oc-rsync.cdx.json",
//!     ])
//!     .status()
//!     .expect("invoke xtask sbom with custom output");
//! ```
//!
//! # See also
//!
//! - [`cargo-cyclonedx`](https://github.com/CycloneDX/cyclonedx-rust-cargo) —
//!   upstream documentation for the SBOM generator invoked by this task.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

fn main() -> ExitCode {
    match run_with_args(env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(TaskError::Help(text)) => {
            println!("{}", text);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", error);
            if let TaskError::Usage(_) = error {
                eprintln!("{}", top_level_usage());
            }
            ExitCode::FAILURE
        }
    }
}

fn run_with_args<I>(args: I) -> Result<(), TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Err(TaskError::Usage(String::from(
            "missing command; run with --help to see available tasks",
        )));
    };

    if is_help_flag(&first) {
        return Err(TaskError::Help(top_level_usage()));
    }

    let command = first.to_string_lossy();
    match command.as_ref() {
        "help" => Err(TaskError::Help(top_level_usage())),
        "sbom" => {
            let options = parse_sbom_args(args)?;
            let workspace = workspace_root()?;
            execute_sbom(&workspace, options)
        }
        other => Err(TaskError::Usage(format!(
            "unrecognised command '{other}'; run with --help for available tasks"
        ))),
    }
}

fn is_help_flag(value: &OsString) -> bool {
    value == "--help" || value == "-h"
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SbomOptions {
    output: Option<PathBuf>,
}

fn parse_sbom_args<I>(args: I) -> Result<SbomOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut output = None;

    while let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(sbom_usage()));
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

fn execute_sbom(workspace: &Path, options: SbomOptions) -> Result<(), TaskError> {
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
    let status = Command::new("cargo")
        .arg("cyclonedx")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .arg("--workspace")
        .arg("--format")
        .arg("json")
        .arg("--output")
        .arg(&output_path)
        .arg("--all-features")
        .arg("--locked")
        .status()
        .map_err(|error| map_command_error(error, "cargo cyclonedx"))?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("cargo cyclonedx"),
            status,
        });
    }

    Ok(())
}

fn workspace_root() -> Result<PathBuf, TaskError> {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        TaskError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "CARGO_MANIFEST_DIR environment variable is not set",
        ))
    })?;
    let mut path = PathBuf::from(manifest_dir);
    if !path.pop() {
        return Err(TaskError::Io(io::Error::new(
            io::ErrorKind::Other,
            "failed to locate workspace root",
        )));
    }
    Ok(path)
}

fn top_level_usage() -> String {
    String::from(
        "Usage: cargo xtask <command>\n\nCommands:\n  sbom    Generate a CycloneDX SBOM (requires cargo-cyclonedx)\n  help    Show this help message\n\nRun `cargo xtask <command> --help` for details about a specific command.",
    )
}

fn sbom_usage() -> String {
    String::from(
        "Usage: cargo xtask sbom [--output PATH]\n\nOptions:\n  --output PATH    Override the SBOM output path (relative to the workspace root unless absolute)\n  -h, --help       Show this help message",
    )
}

#[derive(Debug)]
enum TaskError {
    Usage(String),
    Help(String),
    Io(io::Error),
    ToolMissing(String),
    CommandFailed { program: String, status: ExitStatus },
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Usage(message) | TaskError::Help(message) => f.write_str(message),
            TaskError::Io(error) => write!(f, "{error}"),
            TaskError::ToolMissing(message) => f.write_str(message),
            TaskError::CommandFailed { program, status } => {
                if let Some(code) = status.code() {
                    write!(f, "{program} exited with status code {code}")
                } else {
                    write!(f, "{program} terminated by signal")
                }
            }
        }
    }
}

impl From<io::Error> for TaskError {
    fn from(error: io::Error) -> Self {
        TaskError::Io(error)
    }
}

fn map_command_error(error: io::Error, program: &str) -> TaskError {
    if error.kind() == io::ErrorKind::NotFound {
        TaskError::ToolMissing(format!(
            "{program} is unavailable; install the cargo-cyclonedx plugin to generate SBOMs"
        ))
    } else {
        TaskError::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{SbomOptions, TaskError, parse_sbom_args, sbom_usage, top_level_usage};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn parse_sbom_args_accepts_default_configuration() {
        let options = parse_sbom_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, SbomOptions { output: None });
    }

    #[test]
    fn parse_sbom_args_accepts_custom_output_path() {
        let options = parse_sbom_args([OsString::from("--output"), OsString::from("sbom.json")])
            .expect("parse succeeds");
        assert_eq!(
            options,
            SbomOptions {
                output: Some(PathBuf::from("sbom.json"))
            }
        );
    }

    #[test]
    fn parse_sbom_args_rejects_missing_output_value() {
        let error = parse_sbom_args([OsString::from("--output")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--output")));
    }

    #[test]
    fn parse_sbom_args_rejects_duplicate_output_flags() {
        let error = parse_sbom_args([
            OsString::from("--output"),
            OsString::from("one.json"),
            OsString::from("--output"),
            OsString::from("two.json"),
        ])
        .unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--output")));
    }

    #[test]
    fn parse_sbom_args_rejects_unknown_flags() {
        let error = parse_sbom_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("unknown")));
    }

    #[test]
    fn parse_sbom_args_reports_help_request() {
        let error = parse_sbom_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == sbom_usage()));
    }

    #[test]
    fn top_level_usage_mentions_sbom_command() {
        let usage = top_level_usage();
        assert!(usage.contains("sbom"));
    }
}
