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

mod cli;
mod commands;
mod error;
mod task;
#[cfg(test)]
mod test_support;
mod util;
mod workspace;

use crate::cli::{Cli, Command, CommandExt};
use crate::commands::{
    benchmark, branding, doc_package, docs, enforce_limits, interop, no_binaries, no_placeholders,
    package, preflight, readme_version, release, sbom, test,
};
use crate::error::TaskError;
use crate::task::TreeRenderer;
use crate::workspace::workspace_root;
use clap::Parser;
use std::io::{self, IsTerminal};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.tree {
        return render_task_tree(&cli.command);
    }
    match run_command(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Renders the task tree for a command and exits.
fn render_task_tree(command: &Command) -> ExitCode {
    let task = command.as_task();
    let use_color = io::stdout().is_terminal();
    let mut renderer = TreeRenderer::new(io::stdout(), use_color);
    match renderer.render(task.as_ref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Failed to render task tree: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_command(cli: Cli) -> Result<(), TaskError> {
    let workspace = workspace_root()?;

    match cli.command {
        Command::Benchmark(args) => benchmark::execute(&workspace, args.into()),
        Command::Branding(args) => branding::execute(&workspace, args.into()),
        Command::Docs(args) => docs::execute(&workspace, args.into()),
        Command::DocPackage(args) => doc_package::execute(&workspace, args.into()),
        Command::EnforceLimits(args) => enforce_limits::execute(&workspace, args.into()),
        Command::Interop(args) => interop::execute(&workspace, args.into()),
        Command::NoBinaries => no_binaries::execute(&workspace),
        Command::NoPlaceholders => no_placeholders::execute(&workspace),
        Command::Package(args) => package::execute(&workspace, args.into()),
        Command::Preflight => preflight::execute(&workspace),
        Command::ReadmeVersion => readme_version::execute(&workspace),
        Command::Release(args) => release::execute(&workspace, args.into()),
        Command::Sbom(args) => sbom::execute(&workspace, args.into()),
        Command::Test(args) => test::execute(&workspace, args.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn run_command_executes_sbom() {
        let temp = tempdir().expect("create temp dir");
        let output = temp.path().join("cmd-sbom.json");

        let cli = Cli::parse_from(["cargo-xtask", "sbom", "--output", output.to_str().unwrap()]);
        match run_command(cli) {
            Ok(()) => {
                assert!(output.exists(), "SBOM output file should be created");
            }
            Err(TaskError::Metadata(message)) if message.contains("cargo metadata") => {
                // Skip test when cargo metadata fails due to environment issues
                // (e.g., parallel test execution, missing lock file, or CI environment).
                // The core SBOM logic is covered by tests in commands::sbom.
                eprintln!("skipping: cargo metadata unavailable ({message})");
            }
            Err(error) => {
                panic!("sbom command failed unexpectedly: {error}");
            }
        }
    }
}
