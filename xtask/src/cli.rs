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
    /// Display task tree without executing.
    #[arg(long, global = true)]
    pub tree: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// Available xtask subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run performance benchmarks comparing oc-rsync versions.
    Benchmark(BenchmarkArgs),

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

/// Benchmark mode.
#[derive(Debug, Clone, Copy, Default, ValueEnum, PartialEq, Eq)]
pub enum BenchmarkMode {
    /// Use local rsync daemon (requires kernel source download).
    #[default]
    Local,
    /// Use remote rsync:// servers (public mirrors).
    Remote,
}

/// Arguments for the `benchmark` command.
#[derive(Parser, Debug, Default)]
pub struct BenchmarkArgs {
    /// Directory for benchmark data and daemon.
    #[arg(long, value_name = "DIR")]
    pub bench_dir: Option<std::path::PathBuf>,

    /// Rsync daemon port (local mode only).
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// Number of runs per version.
    #[arg(long, value_name = "N", default_value = "5")]
    pub runs: Option<usize>,

    /// Specific versions to benchmark (default: last 3 releases + dev).
    #[arg(long, value_name = "VER")]
    pub versions: Vec<String>,

    /// Skip building versions (use existing binaries).
    #[arg(long)]
    pub skip_build: bool,

    /// Output results as JSON.
    #[arg(long)]
    pub json: bool,

    /// Benchmark mode: local (daemon) or remote (public mirrors).
    #[arg(long, value_enum, default_value = "local")]
    pub mode: BenchmarkMode,

    /// Custom remote rsync:// URLs to benchmark (can be specified multiple times).
    #[arg(long = "url", value_name = "URL")]
    pub urls: Vec<String>,

    /// List available public rsync mirrors and exit.
    #[arg(long)]
    pub list_mirrors: bool,
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

    /// Build deb using the specified variant (e.g., "focal" for Ubuntu 20.04).
    #[arg(long, value_name = "VARIANT")]
    pub deb_variant: Option<String>,

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

// ─────────────────────────────────────────────────────────────────────────────
// Task tree conversion
// ─────────────────────────────────────────────────────────────────────────────

use crate::task::Task;
use crate::task::tasks::{
    DocPackageTask, DocsTask, EnforceLimitsTask, NoBinariesTask, NoPlaceholdersTask, PackageTask,
    PreflightTask, ReleaseTask, SbomTask, TestTask,
};

/// Extension trait for converting commands to task trees.
pub trait CommandExt {
    /// Converts this command into a task tree for visualization.
    fn as_task(&self) -> Box<dyn Task>;
}

impl CommandExt for Command {
    fn as_task(&self) -> Box<dyn Task> {
        match self {
            Command::Benchmark(args) => args.as_task(),
            Command::Branding(args) => args.as_task(),
            Command::Docs(args) => args.as_task(),
            Command::DocPackage(args) => args.as_task(),
            Command::EnforceLimits(_) => Box::new(EnforceLimitsTask),
            Command::Interop(args) => args.as_task(),
            Command::NoBinaries => Box::new(NoBinariesTask),
            Command::NoPlaceholders => Box::new(NoPlaceholdersTask),
            Command::Package(args) => args.as_task(),
            Command::Preflight => Box::new(PreflightTask),
            Command::ReadmeVersion => Box::new(ReadmeVersionTask),
            Command::Release(args) => args.as_task(),
            Command::Sbom(_) => Box::new(SbomTask),
            Command::Test(args) => args.as_task(),
        }
    }
}

impl CommandExt for BenchmarkArgs {
    fn as_task(&self) -> Box<dyn Task> {
        Box::new(BenchmarkTask)
    }
}

impl CommandExt for BrandingArgs {
    fn as_task(&self) -> Box<dyn Task> {
        Box::new(BrandingTask)
    }
}

impl CommandExt for DocsArgs {
    fn as_task(&self) -> Box<dyn Task> {
        Box::new(DocsTask {
            open: self.open,
            validate: self.validate,
        })
    }
}

impl CommandExt for DocPackageArgs {
    fn as_task(&self) -> Box<dyn Task> {
        Box::new(DocPackageTask { open: self.open })
    }
}

impl CommandExt for InteropArgs {
    fn as_task(&self) -> Box<dyn Task> {
        Box::new(InteropTask)
    }
}

impl CommandExt for PackageArgs {
    fn as_task(&self) -> Box<dyn Task> {
        let build_all = !self.deb && !self.rpm && !self.tarball;
        Box::new(PackageTask {
            build_deb: self.deb || build_all,
            build_rpm: self.rpm || build_all,
            build_tarball: self.tarball || build_all,
            deb_variant: self.deb_variant.clone(),
        })
    }
}

impl CommandExt for ReleaseArgs {
    fn as_task(&self) -> Box<dyn Task> {
        Box::new(ReleaseTask {
            skip_docs: self.skip_docs,
            skip_hygiene: self.skip_hygiene,
            skip_placeholder_scan: self.skip_placeholder_scan,
            skip_binary_scan: self.skip_binary_scan,
            skip_packages: self.skip_packages,
            skip_upload: self.skip_upload,
        })
    }
}

impl CommandExt for TestArgs {
    fn as_task(&self) -> Box<dyn Task> {
        Box::new(TestTask {
            use_nextest: !self.use_cargo_test,
        })
    }
}

// Simple leaf tasks for commands without complex decomposition.

use std::time::Duration;

/// Task for performance benchmarking.
struct BenchmarkTask;

impl Task for BenchmarkTask {
    fn name(&self) -> &'static str {
        "benchmark"
    }

    fn description(&self) -> &'static str {
        "Run performance benchmarks"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(300)) // Benchmarks can take a while
    }
}

/// Task for branding validation.
struct BrandingTask;

impl Task for BrandingTask {
    fn name(&self) -> &'static str {
        "branding"
    }

    fn description(&self) -> &'static str {
        "Validate workspace branding"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }
}

/// Task for interop validation.
struct InteropTask;

impl Task for InteropTask {
    fn name(&self) -> &'static str {
        "interop"
    }

    fn description(&self) -> &'static str {
        "Validate upstream rsync compatibility"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }
}

/// Task for README version validation.
struct ReadmeVersionTask;

impl Task for ReadmeVersionTask {
    fn name(&self) -> &'static str {
        "readme-version"
    }

    fn description(&self) -> &'static str {
        "Validate README version references"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
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
    fn parse_package_deb_variant() {
        let cli = Cli::parse_from(["cargo-xtask", "package", "--deb", "--deb-variant", "focal"]);
        match cli.command {
            Command::Package(args) => {
                assert!(args.deb);
                assert_eq!(args.deb_variant, Some("focal".to_owned()));
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

    #[test]
    fn parse_tree_flag_before_command() {
        let cli = Cli::parse_from(["cargo-xtask", "--tree", "package"]);
        assert!(cli.tree);
    }

    #[test]
    fn parse_tree_flag_after_command() {
        let cli = Cli::parse_from(["cargo-xtask", "package", "--tree"]);
        assert!(cli.tree);
    }

    #[test]
    fn command_ext_package_creates_task() {
        let args = PackageArgs {
            deb: true,
            rpm: false,
            tarball: false,
            ..Default::default()
        };
        let task = args.as_task();
        assert_eq!(task.name(), "package");
    }

    #[test]
    fn command_ext_release_creates_task() {
        let args = ReleaseArgs::default();
        let task = args.as_task();
        assert_eq!(task.name(), "release");
    }

    #[test]
    fn parse_benchmark_remote_mode() {
        let cli = Cli::parse_from(["cargo-xtask", "benchmark", "--mode", "remote"]);
        match cli.command {
            Command::Benchmark(args) => {
                assert_eq!(args.mode, BenchmarkMode::Remote);
            }
            _ => panic!("expected benchmark command"),
        }
    }

    #[test]
    fn parse_benchmark_custom_urls() {
        let cli = Cli::parse_from([
            "cargo-xtask",
            "benchmark",
            "--mode",
            "remote",
            "--url",
            "rsync://example.com/test/",
            "--url",
            "rsync://other.com/data/",
        ]);
        match cli.command {
            Command::Benchmark(args) => {
                assert_eq!(args.mode, BenchmarkMode::Remote);
                assert_eq!(args.urls.len(), 2);
                assert_eq!(args.urls[0], "rsync://example.com/test/");
                assert_eq!(args.urls[1], "rsync://other.com/data/");
            }
            _ => panic!("expected benchmark command"),
        }
    }

    #[test]
    fn parse_benchmark_list_mirrors() {
        let cli = Cli::parse_from(["cargo-xtask", "benchmark", "--list-mirrors"]);
        match cli.command {
            Command::Benchmark(args) => {
                assert!(args.list_mirrors);
            }
            _ => panic!("expected benchmark command"),
        }
    }

    #[test]
    fn parse_benchmark_versions() {
        let cli = Cli::parse_from([
            "cargo-xtask",
            "benchmark",
            "--versions",
            "v0.5.2",
            "--versions",
            "v0.5.3",
        ]);
        match cli.command {
            Command::Benchmark(args) => {
                assert_eq!(args.versions.len(), 2);
                assert_eq!(args.versions[0], "v0.5.2");
                assert_eq!(args.versions[1], "v0.5.3");
            }
            _ => panic!("expected benchmark command"),
        }
    }
}
