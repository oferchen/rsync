#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! # Overview
//!
//! The `xtask` utility hosts workspace maintenance commands that are not part of
//! the shipping binaries. In addition to producing a CycloneDX Software Bill of
//! Materials (SBOM) so packaging automation can ship reproducible metadata
//! alongside the Rust `rsync` binaries, the tool exposes commands that validate
//! workspace branding, documentation, packaging assets, and hygiene policies.
//!
//! Invocations follow the conventional `cargo xtask <command>` pattern. The
//! `sbom` command analyses workspace metadata directly and writes a CycloneDX
//! JSON document to `target/sbom/rsync.cdx.json` unless an explicit output path
//! is provided.
//!
//! # Examples
//!
//! Generate the default SBOM:
//!
//! ```
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
//!         "artifacts/rsync.cdx.json",
//!     ])
//!     .status()
//!     .expect("invoke xtask sbom with custom output");
//! ```
//!
//! # See also
//!
//! - [CycloneDX Specification](https://cyclonedx.org/specification/) — format
//!   reference for the generated Software Bill of Materials.

mod commands;
mod error;
#[cfg(test)]
mod test_support;
mod util;
mod workspace;

use crate::commands::{
    branding, docs, enforce_limits, no_binaries, no_placeholders, package, preflight,
    readme_version, release, sbom, test,
};
use crate::error::TaskError;
use crate::util::is_help_flag;
use crate::workspace::workspace_root;
use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run_with_args(env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(TaskError::Help(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
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
        "branding" => {
            let options = branding::parse_args(args)?;
            let workspace = workspace_root()?;
            branding::execute(&workspace, options)
        }
        "docs" => {
            let options = docs::parse_args(args)?;
            let workspace = workspace_root()?;
            docs::execute(&workspace, options)
        }
        "no-binaries" => {
            let options = no_binaries::parse_args(args)?;
            let workspace = workspace_root()?;
            no_binaries::execute(&workspace, options)
        }
        "enforce-limits" => {
            let options = enforce_limits::parse_args(args)?;
            let workspace = workspace_root()?;
            enforce_limits::execute(&workspace, options)
        }
        "no-placeholders" => {
            let options = no_placeholders::parse_args(args)?;
            let workspace = workspace_root()?;
            no_placeholders::execute(&workspace, options)
        }
        "preflight" => {
            let options = preflight::parse_args(args)?;
            let workspace = workspace_root()?;
            preflight::execute(&workspace, options)
        }
        "release" => {
            let options = release::parse_args(args)?;
            let workspace = workspace_root()?;
            release::execute(&workspace, options)
        }
        "sbom" => {
            let options = sbom::parse_args(args)?;
            let workspace = workspace_root()?;
            sbom::execute(&workspace, options)
        }
        "readme-version" => {
            let options = readme_version::parse_args(args)?;
            let workspace = workspace_root()?;
            readme_version::execute(&workspace, options)
        }
        "package" => {
            let options = package::parse_args(args)?;
            let workspace = workspace_root()?;
            package::execute(&workspace, options)
        }
        "test" => {
            let options = test::parse_args(args)?;
            let workspace = workspace_root()?;
            test::execute(&workspace, options)
        }
        other => Err(TaskError::Usage(format!(
            "unrecognised command '{other}'; run with --help for available tasks"
        ))),
    }
}

fn top_level_usage() -> String {
    String::from(concat!(
        "Usage: cargo xtask <command>\n\nCommands:\n",
        "  branding         Validate workspace branding metadata\n",
        "  docs            Build API docs and run doctests\n",
        "  enforce-limits   Enforce source line and comment hygiene limits\n",
        "  no-binaries      Assert the git index contains no binary artifacts\n",
        "  no-placeholders  Ensure Rust sources are free from placeholder code\n",
        "  package         Build distribution artifacts (deb/rpm)\n",
        "  preflight        Run packaging preflight validation\n",
        "  release         Run aggregated release-readiness checks\n",
        "  readme-version   Ensure README versions match workspace metadata\n",
        "  test            Run the workspace test suite (prefers cargo-nextest)\n",
        "  sbom             Generate a CycloneDX SBOM for the workspace\n",
        "  help             Show this help message\n\n",
        "Run `cargo xtask <command> --help` for command-specific options."
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn top_level_usage_mentions_enforce_limits_command() {
        let usage = top_level_usage();
        assert!(usage.contains("enforce-limits"));
        assert!(usage.contains("readme-version"));
        assert!(usage.contains("release"));
        assert!(usage.contains("test"));
    }

    #[test]
    fn run_with_args_requires_command() {
        let error = run_with_args(std::iter::empty()).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("missing command")));
    }

    #[test]
    fn run_with_args_reports_help_for_help_command() {
        let error = run_with_args([OsString::from("help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message.contains("Usage")));
    }

    #[test]
    fn run_with_args_reports_unknown_command() {
        let error = run_with_args([OsString::from("unknown")]).unwrap_err();
        assert!(
            matches!(error, TaskError::Usage(message) if message.contains("unrecognised command"))
        );
    }

    #[test]
    fn run_with_args_executes_sbom_command() {
        let temp = tempdir().expect("create temp dir");
        let output = temp.path().join("cmd-sbom.json");
        run_with_args([
            OsString::from("sbom"),
            OsString::from("--output"),
            output.clone().into_os_string(),
        ])
        .expect("sbom command succeeds");
        assert!(output.exists());
    }
}
