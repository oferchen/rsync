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
#[cfg(test)]
mod test_support;
mod util;
mod workspace;

use crate::cli::{Cli, Command, InteropCommand};
use crate::commands::{
    branding, doc_package, docs, enforce_limits, interop, no_binaries, no_placeholders, package,
    preflight, readme_version, release, sbom, test,
};
use crate::error::TaskError;
use crate::workspace::workspace_root;
use clap::Parser;
use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run_command(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_command(cli: Cli) -> Result<(), TaskError> {
    let workspace = workspace_root()?;

    match cli.command {
        Command::Branding(args) => {
            let options = branding::BrandingOptions {
                format: if args.json {
                    branding::BrandingOutputFormat::Json
                } else {
                    branding::BrandingOutputFormat::Text
                },
            };
            branding::execute(&workspace, options)
        }

        Command::Docs(args) => {
            let options = docs::DocsOptions {
                open: args.open,
                validate: args.validate,
            };
            docs::execute(&workspace, options)
        }

        Command::DocPackage(args) => {
            let options = doc_package::DocPackageOptions {
                output: args.output,
                open: args.open,
            };
            doc_package::execute(&workspace, options)
        }

        Command::EnforceLimits(args) => {
            // Validate warn_lines <= max_lines if both provided
            if let (Some(warn), Some(max)) = (args.warn_lines, args.max_lines) {
                if warn > max {
                    return Err(TaskError::Usage(format!(
                        "warn line limit ({warn}) cannot exceed maximum line limit ({max})"
                    )));
                }
            }
            let options = enforce_limits::EnforceLimitsOptions {
                max_lines: args.max_lines,
                warn_lines: args.warn_lines,
                config_path: args.config,
            };
            enforce_limits::execute(&workspace, options)
        }

        Command::Interop(args) => {
            let command = args.command.unwrap_or(InteropCommand::All);
            let options = match command {
                InteropCommand::ExitCodes(common) => interop::InteropOptions {
                    command: interop::InteropCommand::ExitCodes(interop::ExitCodesOptions {
                        regenerate: common.regenerate,
                        version: common.version,
                        verbose: common.verbose,
                        implementation: common.implementation,
                        show_output: common.show_output,
                        log_dir: common.log_dir,
                    }),
                },
                InteropCommand::Messages(common) => interop::InteropOptions {
                    command: interop::InteropCommand::Messages(interop::MessagesOptions {
                        regenerate: common.regenerate,
                        version: common.version,
                        verbose: common.verbose,
                        implementation: common.implementation,
                        show_output: common.show_output,
                        log_dir: common.log_dir,
                    }),
                },
                InteropCommand::All => interop::InteropOptions {
                    command: interop::InteropCommand::All,
                },
            };
            interop::execute(&workspace, options)
        }

        Command::NoBinaries(_) => no_binaries::execute(&workspace, no_binaries::NoBinariesOptions),

        Command::NoPlaceholders(_) => {
            no_placeholders::execute(&workspace, no_placeholders::NoPlaceholdersOptions)
        }

        Command::Package(args) => {
            let profile = if args.no_profile {
                None
            } else if args.debug {
                Some(OsString::from("debug"))
            } else if let Some(ref name) = args.profile {
                Some(OsString::from(name))
            } else {
                // Default to dist profile (--release is also default)
                Some(OsString::from(package::DIST_PROFILE))
            };

            let (build_deb, build_rpm, build_tarball) = if !args.deb && !args.rpm && !args.tarball {
                // Default to building all
                (true, true, true)
            } else {
                (args.deb, args.rpm, args.tarball)
            };

            let options = package::PackageOptions {
                build_deb,
                build_rpm,
                build_tarball,
                tarball_target: args.tarball_target.map(OsString::from),
                profile,
            };
            package::execute(&workspace, options)
        }

        Command::Preflight(_) => preflight::execute(&workspace, preflight::PreflightOptions),

        Command::ReadmeVersion(_) => {
            readme_version::execute(&workspace, readme_version::ReadmeVersionOptions)
        }

        Command::Release(args) => {
            let options = release::ReleaseOptions {
                skip_docs: args.skip_docs,
                skip_hygiene: args.skip_hygiene,
                skip_placeholder_scan: args.skip_placeholder_scan,
                skip_binary_scan: args.skip_binary_scan,
                skip_packages: args.skip_packages,
                skip_upload: args.skip_upload,
            };
            release::execute(&workspace, options)
        }

        Command::Sbom(args) => {
            let options = sbom::SbomOptions {
                output: args.output,
            };
            sbom::execute(&workspace, options)
        }

        Command::Test(args) => {
            let options = test::TestOptions {
                force_cargo_test: args.use_cargo_test,
                install_nextest: args.install_nextest,
            };
            test::execute(&workspace, options)
        }
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
        run_command(cli).expect("sbom command succeeds");
        assert!(output.exists());
    }
}
