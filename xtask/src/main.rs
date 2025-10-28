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
//!         "artifacts/rsync.cdx.json",
//!     ])
//!     .status()
//!     .expect("invoke xtask sbom with custom output");
//! ```
//!
//! # See also
//!
//! - [`cargo-cyclonedx`](https://github.com/CycloneDX/cyclonedx-rust-cargo) —
//!   upstream documentation for the SBOM generator invoked by this task.

mod commands;
mod error;
mod util;
mod workspace;

use crate::commands::{
    branding, docs, enforce_limits, no_binaries, no_placeholders, package, preflight, sbom,
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
        "sbom" => {
            let options = sbom::parse_args(args)?;
            let workspace = workspace_root()?;
            sbom::execute(&workspace, options)
        }
        "package" => {
            let options = package::parse_args(args)?;
            let workspace = workspace_root()?;
            package::execute(&workspace, options)
        }
        other => Err(TaskError::Usage(format!(
            "unrecognised command '{other}'; run with --help for available tasks"
        ))),
    }
}

fn top_level_usage() -> String {
    String::from(
        "Usage: cargo xtask <command>\n\nCommands:\n  branding         Validate workspace branding metadata\n  docs             Build API docs and run doctests\n  enforce-limits   Enforce source line and comment hygiene limits\n  no-binaries      Assert the git index contains no binary artifacts\n  no-placeholders  Ensure Rust sources are free from placeholder code\n  package          Build distribution artifacts (deb/rpm)\n  preflight        Run packaging preflight validation\n  sbom             Generate a CycloneDX SBOM for the workspace\n  help             Show this help message\n\nRun `cargo xtask <command> --help` for command-specific options.",
    )
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_usage_mentions_enforce_limits_command() {
        let usage = top_level_usage();
        assert!(usage.contains("enforce-limits"));
    }
}
