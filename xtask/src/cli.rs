//! CLI argument parsing using clap.
//!
//! This module provides the command-line interface definition for xtask using
//! clap's derive macros. It replaces the previous manual argument parsing with
//! a declarative, type-safe approach.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Workspace maintenance commands for oc-rsync.
///
/// The `xtask` utility hosts workspace maintenance commands that are not part of
/// the shipping binaries. Run `cargo xtask <command> --help` for command-specific
/// options.
#[derive(Parser, Debug)]
#[command(name = "cargo xtask")]
#[command(about = "Workspace maintenance commands for oc-rsync")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Available xtask subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Validate workspace branding metadata.
    Branding(BrandingArgs),

    /// Build API docs and run doctests.
    Docs(DocsArgs),

    /// Generate and package rustdoc for distribution.
    DocPackage(DocPackageArgs),

    /// Enforce source line and comment hygiene limits.
    EnforceLimits(EnforceLimitsArgs),

    /// Validate interoperability with upstream rsync.
    Interop(InteropArgs),

    /// Assert the git index contains no binary artifacts.
    NoBinaries,

    /// Ensure Rust sources are free from placeholder code.
    NoPlaceholders,

    /// Build distribution artifacts (deb/rpm/tarball).
    Package(PackageArgs),

    /// Run packaging preflight validation.
    Preflight,

    /// Ensure README versions match workspace metadata.
    ReadmeVersion,

    /// Run aggregated release-readiness checks.
    Release(ReleaseArgs),

    /// Generate a CycloneDX SBOM for the workspace.
    Sbom(SbomArgs),

    /// Run the workspace test suite (prefers cargo-nextest).
    Test(TestArgs),
}

/// Output format for branding command.
#[derive(Debug, Clone, Copy, Default, ValueEnum, PartialEq, Eq)]
pub enum BrandingFormat {
    /// Human-readable text report.
    #[default]
    Text,
    /// Structured JSON report suitable for automation.
    Json,
}

/// Arguments for the `branding` command.
#[derive(Parser, Debug, Default)]
pub struct BrandingArgs {
    /// Emit branding metadata in JSON format.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for the `docs` command.
#[derive(Parser, Debug, Default)]
pub struct DocsArgs {
    /// Open documentation after building.
    #[arg(long)]
    pub open: bool,

    /// Validate branding references in Markdown documents.
    #[arg(long)]
    pub validate: bool,
}

/// Arguments for the `doc-package` command.
#[derive(Parser, Debug)]
pub struct DocPackageArgs {
    /// Output directory for the documentation tarball.
    #[arg(short, long, default_value = "target/doc-dist")]
    pub output: PathBuf,

    /// Open documentation in browser after building.
    #[arg(long)]
    pub open: bool,
}

impl Default for DocPackageArgs {
    fn default() -> Self {
        Self {
            output: PathBuf::from("target/doc-dist"),
            open: false,
        }
    }
}

/// Arguments for the `enforce-limits` command.
#[derive(Parser, Debug, Default)]
pub struct EnforceLimitsArgs {
    /// Fail when a Rust source exceeds N lines.
    #[arg(long, value_name = "N")]
    pub max_lines: Option<usize>,

    /// Warn when a Rust source exceeds N lines.
    #[arg(long, value_name = "N")]
    pub warn_lines: Option<usize>,

    /// Override the line limit configuration path.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// Arguments for the `interop` command.
#[derive(Parser, Debug)]
pub struct InteropArgs {
    #[command(subcommand)]
    pub command: Option<InteropCommand>,
}

/// Interop subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum InteropCommand {
    /// Validate exit codes against upstream rsync.
    ExitCodes(InteropCommonArgs),

    /// Validate message formats against upstream rsync.
    Messages(InteropCommonArgs),

    /// Run all validations (exit codes + messages).
    All,
}

/// Common arguments for interop subcommands.
#[derive(Parser, Debug, Clone, Default)]
pub struct InteropCommonArgs {
    /// Regenerate golden files instead of validating.
    #[arg(long)]
    pub regenerate: bool,

    /// Test against specific upstream version (3.0.9, 3.1.3, 3.4.1).
    #[arg(long, value_name = "VER")]
    pub version: Option<String>,

    /// Implementation to test: "upstream" (default) or "oc-rsync".
    #[arg(long = "impl", value_name = "IMPL")]
    pub implementation: Option<String>,

    /// Enable verbose output.
    #[arg(short, long)]
    pub verbose: bool,

    /// Show stdout/stderr from rsync commands.
    #[arg(short = 'o', long)]
    pub show_output: bool,

    /// Save rsync logs to directory (uses rsync --log-file).
    #[arg(long, value_name = "DIR")]
    pub log_dir: Option<String>,
}

/// Arguments for the `package` command.
#[derive(Parser, Debug, Default)]
pub struct PackageArgs {
    /// Build only the Debian package.
    #[arg(long)]
    pub deb: bool,

    /// Build only the RPM package.
    #[arg(long)]
    pub rpm: bool,

    /// Build only the tar.gz distributions.
    #[arg(long)]
    pub tarball: bool,

    /// Restrict tarball generation to the specified target triple.
    #[arg(long, value_name = "TARGET")]
    pub tarball_target: Option<String>,

    /// Build using the dist profile (default).
    #[arg(long, group = "profile_group")]
    pub release: bool,

    /// Build using the debug profile.
    #[arg(long, group = "profile_group")]
    pub debug: bool,

    /// Build using the named cargo profile.
    #[arg(long, value_name = "NAME", group = "profile_group")]
    pub profile: Option<String>,

    /// Do not override the cargo profile.
    #[arg(long, group = "profile_group")]
    pub no_profile: bool,
}

/// Arguments for the `release` command.
#[derive(Parser, Debug, Default)]
pub struct ReleaseArgs {
    /// Skip building docs and running doctests.
    #[arg(long)]
    pub skip_docs: bool,

    /// Skip enforce-limits line-count checks.
    #[arg(long)]
    pub skip_hygiene: bool,

    /// Skip placeholder detection scans.
    #[arg(long)]
    pub skip_placeholder_scan: bool,

    /// Skip checking the git index for binary files.
    #[arg(long)]
    pub skip_binary_scan: bool,

    /// Skip building release packages.
    #[arg(long)]
    pub skip_packages: bool,

    /// Skip uploading release packages to GitHub.
    #[arg(long)]
    pub skip_upload: bool,
}

/// Arguments for the `sbom` command.
#[derive(Parser, Debug, Default)]
pub struct SbomArgs {
    /// Override the SBOM output path (relative to workspace root unless absolute).
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

/// Arguments for the `test` command.
#[derive(Parser, Debug, Default)]
pub struct TestArgs {
    /// Force running cargo test even when cargo-nextest is available.
    #[arg(long)]
    pub use_cargo_test: bool,

    /// Install cargo-nextest when missing before falling back to cargo test.
    #[arg(long)]
    pub install_nextest: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli_structure() {
        // This test ensures the CLI structure is valid
        Cli::command().debug_assert();
    }

    #[test]
    fn parse_branding_json_flag() {
        let cli = Cli::parse_from(["cargo-xtask", "branding", "--json"]);
        match cli.command {
            Command::Branding(args) => assert!(args.json),
            _ => panic!("expected branding command"),
        }
    }

    #[test]
    fn parse_sbom_output_flag() {
        let cli = Cli::parse_from(["cargo-xtask", "sbom", "--output", "custom.json"]);
        match cli.command {
            Command::Sbom(args) => {
                assert_eq!(args.output, Some(PathBuf::from("custom.json")));
            }
            _ => panic!("expected sbom command"),
        }
    }

    #[test]
    fn parse_interop_exit_codes() {
        let cli = Cli::parse_from([
            "cargo-xtask",
            "interop",
            "exit-codes",
            "--regenerate",
            "--version",
            "3.4.1",
        ]);
        match cli.command {
            Command::Interop(args) => match args.command {
                Some(InteropCommand::ExitCodes(common)) => {
                    assert!(common.regenerate);
                    assert_eq!(common.version, Some("3.4.1".to_owned()));
                }
                _ => panic!("expected exit-codes subcommand"),
            },
            _ => panic!("expected interop command"),
        }
    }

    #[test]
    fn parse_package_tarball_target() {
        let cli = Cli::parse_from([
            "cargo-xtask",
            "package",
            "--tarball",
            "--tarball-target",
            "x86_64-unknown-linux-gnu",
        ]);
        match cli.command {
            Command::Package(args) => {
                assert!(args.tarball);
                assert_eq!(
                    args.tarball_target,
                    Some("x86_64-unknown-linux-gnu".to_owned())
                );
            }
            _ => panic!("expected package command"),
        }
    }

    #[test]
    fn parse_release_skip_flags() {
        let cli = Cli::parse_from([
            "cargo-xtask",
            "release",
            "--skip-docs",
            "--skip-hygiene",
            "--skip-upload",
        ]);
        match cli.command {
            Command::Release(args) => {
                assert!(args.skip_docs);
                assert!(args.skip_hygiene);
                assert!(args.skip_upload);
                assert!(!args.skip_packages);
            }
            _ => panic!("expected release command"),
        }
    }

    #[test]
    fn parse_enforce_limits_options() {
        let cli = Cli::parse_from([
            "cargo-xtask",
            "enforce-limits",
            "--max-lines",
            "700",
            "--warn-lines",
            "500",
        ]);
        match cli.command {
            Command::EnforceLimits(args) => {
                assert_eq!(args.max_lines, Some(700));
                assert_eq!(args.warn_lines, Some(500));
            }
            _ => panic!("expected enforce-limits command"),
        }
    }

    #[test]
    fn parse_test_options() {
        let cli = Cli::parse_from(["cargo-xtask", "test", "--use-cargo-test"]);
        match cli.command {
            Command::Test(args) => {
                assert!(args.use_cargo_test);
                assert!(!args.install_nextest);
            }
            _ => panic!("expected test command"),
        }
    }
}
