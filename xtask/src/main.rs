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

    if util::is_help_flag(&first) {
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

fn is_help_flag(value: &OsString) -> bool {
    value == "--help" || value == "-h"
}

/// Output format supported by the `branding` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum BrandingOutputFormat {
    /// Human-readable text report.
    #[default]
    Text,
    /// Structured JSON report suitable for automation.
    Json,
}

/// Options accepted by the `branding` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BrandingOptions {
    /// Desired output format.
    format: BrandingOutputFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SbomOptions {
    output: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DocsOptions {
    open: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NoBinariesOptions;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EnforceLimitsOptions {
    max_lines: Option<usize>,
    warn_lines: Option<usize>,
    config_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NoPlaceholdersOptions;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PreflightOptions;

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageOptions {
    build_deb: bool,
    build_rpm: bool,
    profile: Option<OsString>,
}

fn parse_branding_args<I>(args: I) -> Result<BrandingOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = BrandingOptions::default();

    for arg in args {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(branding_usage()));
        }

        let Some(raw) = arg.to_str() else {
            return Err(TaskError::Usage(String::from(
                "branding command arguments must be valid UTF-8",
            )));
        };

        match raw {
            "--json" => {
                if !matches!(options.format, BrandingOutputFormat::Text) {
                    return Err(TaskError::Usage(String::from(
                        "--json specified multiple times",
                    )));
                }

                options.format = BrandingOutputFormat::Json;
            }
            _ => {
                return Err(TaskError::Usage(format!(
                    "unrecognised argument '{}' for branding command",
                    raw
                )));
            }
        }
    }

    Ok(options)
}

fn parse_docs_args<I>(args: I) -> Result<DocsOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = DocsOptions::default();

    for arg in args {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(docs_usage()));
        }

        if arg == "--open" {
            if options.open {
                return Err(TaskError::Usage(String::from(
                    "--open specified multiple times",
                )));
            }

            options.open = true;
            continue;
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for docs command",
            arg.to_string_lossy()
        )));
    }

    Ok(options)
}

fn parse_no_binaries_args<I>(args: I) -> Result<NoBinariesOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(no_binaries_usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for no-binaries command",
            arg.to_string_lossy()
        )));
    }

    Ok(NoBinariesOptions)
}

fn parse_enforce_limits_args<I>(args: I) -> Result<EnforceLimitsOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut options = EnforceLimitsOptions::default();

    while let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(enforce_limits_usage()));
        }

        match arg.to_string_lossy().as_ref() {
            "--max-lines" => {
                let value = args.next().ok_or_else(|| {
                    TaskError::Usage(String::from(
                        "--max-lines requires a positive integer value",
                    ))
                })?;
                let parsed = parse_positive_usize_arg(&value, "--max-lines")?;
                options.max_lines = Some(parsed);
            }
            "--warn-lines" => {
                let value = args.next().ok_or_else(|| {
                    TaskError::Usage(String::from(
                        "--warn-lines requires a positive integer value",
                    ))
                })?;
                let parsed = parse_positive_usize_arg(&value, "--warn-lines")?;
                options.warn_lines = Some(parsed);
            }
            "--config" => {
                let value = args.next().ok_or_else(|| {
                    TaskError::Usage(String::from("--config requires a path argument"))
                })?;
                if value.is_empty() {
                    return Err(TaskError::Usage(String::from(
                        "--config requires a non-empty path argument",
                    )));
                }
                options.config_path = Some(PathBuf::from(value));
            }
            other => {
                return Err(TaskError::Usage(format!(
                    "unrecognised argument '{other}' for enforce-limits command"
                )));
            }
        }
    }

    if let (Some(warn), Some(max)) = (options.warn_lines, options.max_lines) {
        if warn > max {
            return Err(TaskError::Usage(format!(
                "warn line limit ({warn}) cannot exceed maximum line limit ({max})"
            )));
        }
    }

    Ok(options)
}

fn parse_no_placeholders_args<I>(args: I) -> Result<NoPlaceholdersOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(no_placeholders_usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for no-placeholders command",
            arg.to_string_lossy()
        )));
    }

    Ok(NoPlaceholdersOptions)
}

fn parse_preflight_args<I>(args: I) -> Result<PreflightOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(preflight_usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for preflight command",
            arg.to_string_lossy()
        )));
    }

    Ok(PreflightOptions)
}

fn parse_sbom_args<I>(args: I) -> Result<SbomOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut output = None;

    loop {
        let Some(arg) = args.next() else {
            break;
        };

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

fn execute_branding(workspace: &Path, options: BrandingOptions) -> Result<(), TaskError> {
    let branding = load_workspace_branding(workspace)?;
    let output = render_branding(&branding, options.format)?;

    println!("{output}");

    Ok(())
}

fn render_branding(
    branding: &WorkspaceBranding,
    format: BrandingOutputFormat,
) -> Result<String, TaskError> {
    match format {
        BrandingOutputFormat::Text => Ok(render_branding_text(branding)),
        BrandingOutputFormat::Json => render_branding_json(branding),
    }
}

fn render_branding_text(branding: &WorkspaceBranding) -> String {
    format!(
        concat!(
            "Workspace branding summary:\n",
            "  brand: {}\n",
            "  upstream_version: {}\n",
            "  rust_version: {}\n",
            "  protocol: {}\n",
            "  client_bin: {}\n",
            "  daemon_bin: {}\n",
            "  legacy_client_bin: {}\n",
            "  legacy_daemon_bin: {}\n",
            "  daemon_config_dir: {}\n",
            "  daemon_config: {}\n",
            "  daemon_secrets: {}\n",
            "  legacy_daemon_config_dir: {}\n",
            "  legacy_daemon_config: {}\n",
            "  legacy_daemon_secrets: {}\n",
            "  source: {}"
        ),
        branding.brand,
        branding.upstream_version,
        branding.rust_version,
        branding.protocol,
        branding.client_bin,
        branding.daemon_bin,
        branding.legacy_client_bin,
        branding.legacy_daemon_bin,
        branding.daemon_config_dir,
        branding.daemon_config,
        branding.daemon_secrets,
        branding.legacy_daemon_config_dir,
        branding.legacy_daemon_config,
        branding.legacy_daemon_secrets,
        branding.source,
    )
}

fn render_branding_json(branding: &WorkspaceBranding) -> Result<String, TaskError> {
    let value = json!({
        "brand": branding.brand,
        "upstream_version": branding.upstream_version,
        "rust_version": branding.rust_version,
        "protocol": branding.protocol,
        "client_bin": branding.client_bin,
        "daemon_bin": branding.daemon_bin,
        "legacy_client_bin": branding.legacy_client_bin,
        "legacy_daemon_bin": branding.legacy_daemon_bin,
        "daemon_config_dir": branding.daemon_config_dir,
        "daemon_config": branding.daemon_config,
        "daemon_secrets": branding.daemon_secrets,
        "legacy_daemon_config_dir": branding.legacy_daemon_config_dir,
        "legacy_daemon_config": branding.legacy_daemon_config,
        "legacy_daemon_secrets": branding.legacy_daemon_secrets,
        "source": branding.source,
    });

    serde_json::to_string_pretty(&value).map_err(|error| {
        TaskError::Metadata(format!(
            "failed to serialise branding metadata as JSON: {error}"
        ))
    })
}

fn execute_docs(workspace: &Path, options: DocsOptions) -> Result<(), TaskError> {
    println!("Building API documentation");
    let mut doc_args = vec![
        OsString::from("doc"),
        OsString::from("--workspace"),
        OsString::from("--no-deps"),
        OsString::from("--locked"),
    ];
    if options.open {
        doc_args.push(OsString::from("--open"));
    }

    run_cargo_tool(
        workspace,
        doc_args,
        "cargo doc",
        "ensure the Rust toolchain is installed",
    )?;

    println!("Running doctests");
    let test_args = vec![
        OsString::from("test"),
        OsString::from("--doc"),
        OsString::from("--workspace"),
        OsString::from("--locked"),
    ];

    run_cargo_tool(
        workspace,
        test_args,
        "cargo test --doc",
        "ensure the Rust toolchain is installed",
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
