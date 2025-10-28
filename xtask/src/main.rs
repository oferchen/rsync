#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! # Overview
//!
//! The `xtask` utility hosts workspace maintenance commands that are not part of
//! the shipping binaries. The current implementation focuses on producing a
//! CycloneDX Software Bill of Materials (SBOM) so packaging automation can ship
//! reproducible metadata alongside the Rust `rsync` binaries.
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

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};
use toml::Value;

/// Branding metadata extracted from the workspace manifest.
///
/// The struct mirrors the `[workspace.metadata.oc_rsync]` section in the root
/// `Cargo.toml`. Centralising the parse logic ensures packaging and release
/// tooling consume the same authoritative identifiers that the binaries and
/// documentation reference, preventing branding or version drift across the
/// repository.
#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkspaceBranding {
    /// Short brand label (e.g. `"oc"`).
    brand: String,
    /// Upstream rsync version identifier (`"3.4.1"`).
    upstream_version: String,
    /// Rust-branded version identifier (`"3.4.1-rust"`).
    rust_version: String,
    /// Highest protocol version supported by the build.
    protocol: u16,
    /// Canonical client binary name.
    client_bin: String,
    /// Canonical daemon binary name.
    daemon_bin: String,
    /// Legacy upstream-compatible client name.
    legacy_client_bin: String,
    /// Legacy upstream-compatible daemon name.
    legacy_daemon_bin: String,
    /// Directory that houses daemon configuration files.
    daemon_config_dir: String,
    /// Primary daemon configuration file path.
    daemon_config: String,
    /// Primary daemon secrets file path.
    daemon_secrets: String,
    /// Project source URL advertised in documentation and banners.
    source: String,
}

impl WorkspaceBranding {
    /// Returns a concise human-readable summary.
    fn summary(&self) -> String {
        format!(
            "brand={} rust_version={} protocol={} client={} daemon={}",
            self.brand, self.rust_version, self.protocol, self.client_bin, self.daemon_bin
        )
    }
}

/// Builds the path to the workspace manifest.
fn workspace_manifest_path(workspace: &Path) -> PathBuf {
    workspace.join("Cargo.toml")
}

/// Reads the workspace manifest into memory.
fn read_workspace_manifest(workspace: &Path) -> Result<String, TaskError> {
    let manifest_path = workspace_manifest_path(workspace);
    fs::read_to_string(&manifest_path).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!(
                "failed to read workspace manifest at {}: {error}",
                manifest_path.display()
            ),
        ))
    })
}

/// Loads the workspace branding metadata from the manifest on disk.
fn load_workspace_branding(workspace: &Path) -> Result<WorkspaceBranding, TaskError> {
    let manifest = read_workspace_manifest(workspace)?;
    parse_workspace_branding(&manifest)
}

/// Parses the branding metadata from the provided manifest contents.
fn parse_workspace_branding(manifest: &str) -> Result<WorkspaceBranding, TaskError> {
    let value = manifest.parse::<Value>().map_err(|error| {
        TaskError::Metadata(format!("failed to parse workspace manifest: {error}"))
    })?;

    let workspace = value
        .get("workspace")
        .ok_or_else(|| metadata_error("missing [workspace] table"))?;
    let metadata = workspace
        .get("metadata")
        .ok_or_else(|| metadata_error("missing [workspace.metadata] table"))?;
    let oc = metadata
        .get("oc_rsync")
        .ok_or_else(|| metadata_error("missing [workspace.metadata.oc_rsync] table"))?;

    Ok(WorkspaceBranding {
        brand: metadata_str(oc, "brand")?,
        upstream_version: metadata_str(oc, "upstream_version")?,
        rust_version: metadata_str(oc, "rust_version")?,
        protocol: metadata_protocol(oc)?,
        client_bin: metadata_str(oc, "client_bin")?,
        daemon_bin: metadata_str(oc, "daemon_bin")?,
        legacy_client_bin: metadata_str(oc, "legacy_client_bin")?,
        legacy_daemon_bin: metadata_str(oc, "legacy_daemon_bin")?,
        daemon_config_dir: metadata_str(oc, "daemon_config_dir")?,
        daemon_config: metadata_str(oc, "daemon_config")?,
        daemon_secrets: metadata_str(oc, "daemon_secrets")?,
        source: metadata_str(oc, "source")?,
    })
}

fn metadata_str(table: &Value, key: &str) -> Result<String, TaskError> {
    table
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| metadata_error(format!("missing or non-string metadata field '{key}'")))
}

fn metadata_protocol(table: &Value) -> Result<u16, TaskError> {
    let value = table
        .get("protocol")
        .and_then(Value::as_integer)
        .ok_or_else(|| metadata_error("missing or non-integer metadata field 'protocol'"))?;
    u16::try_from(value).map_err(|_| metadata_error("protocol value must fit into u16"))
}

fn metadata_error(message: impl Into<String>) -> TaskError {
    TaskError::Metadata(message.into())
}

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
        "package" => {
            let options = parse_package_args(args)?;
            let workspace = workspace_root()?;
            execute_package(&workspace, options)
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageOptions {
    build_deb: bool,
    build_rpm: bool,
    profile: Option<OsString>,
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
    let mut args = Vec::new();
    args.push(OsString::from("cyclonedx"));
    args.push(OsString::from("--manifest-path"));
    args.push(manifest_path.into_os_string());
    args.push(OsString::from("--workspace"));
    args.push(OsString::from("--format"));
    args.push(OsString::from("json"));
    args.push(OsString::from("--output"));
    args.push(output_path.into_os_string());
    args.push(OsString::from("--all-features"));
    args.push(OsString::from("--locked"));

    run_cargo_tool(
        workspace,
        args,
        "cargo cyclonedx",
        "install the cargo-cyclonedx subcommand (cargo install cargo-cyclonedx)",
    )
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
        "Usage: cargo xtask <command>\n\nCommands:\n  package Build Debian and RPM packages (requires cargo-deb and cargo-rpm)\n  sbom    Generate a CycloneDX SBOM (requires cargo-cyclonedx)\n  help    Show this help message\n\nRun `cargo xtask <command> --help` for details about a specific command.",
    )
}

fn sbom_usage() -> String {
    String::from(
        "Usage: cargo xtask sbom [--output PATH]\n\nOptions:\n  --output PATH    Override the SBOM output path (relative to the workspace root unless absolute)\n  -h, --help       Show this help message",
    )
}

fn package_usage() -> String {
    String::from(
        "Usage: cargo xtask package [OPTIONS]\n\nOptions:\n  --deb            Build only the Debian package\n  --rpm            Build only the RPM package\n  --release        Build using the release profile (default)\n  --debug          Build using the debug profile\n  --profile NAME   Build using the named cargo profile\n  --no-profile     Do not override the cargo profile\n  -h, --help       Show this help message",
    )
}

#[derive(Debug)]
enum TaskError {
    Usage(String),
    Help(String),
    Io(io::Error),
    ToolMissing(String),
    Metadata(String),
    CommandFailed { program: String, status: ExitStatus },
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Usage(message) | TaskError::Help(message) => f.write_str(message),
            TaskError::Io(error) => write!(f, "{error}"),
            TaskError::ToolMissing(message) => f.write_str(message),
            TaskError::Metadata(message) => f.write_str(message),
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

fn map_command_error(error: io::Error, program: &str, install_hint: &str) -> TaskError {
    if error.kind() == io::ErrorKind::NotFound {
        TaskError::ToolMissing(format!("{program} is unavailable; {install_hint}"))
    } else {
        TaskError::Io(error)
    }
}

fn run_cargo_tool(
    workspace: &Path,
    args: Vec<OsString>,
    display: &str,
    install_hint: &str,
) -> Result<(), TaskError> {
    let output = Command::new("cargo")
        .current_dir(workspace)
        .args(&args)
        .output()
        .map_err(|error| map_command_error(error, display, install_hint))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no such subcommand") || stderr.contains("no such command") {
        return Err(TaskError::ToolMissing(format!(
            "{display} is unavailable; {install_hint}"
        )));
    }

    Err(TaskError::CommandFailed {
        program: display.to_string(),
        status: output.status,
    })
}

fn parse_package_args<I>(args: I) -> Result<PackageOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut build_deb = false;
    let mut build_rpm = false;
    let mut profile = Some(OsString::from("release"));
    let mut profile_explicit = false;

    while let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(package_usage()));
        }

        if arg == "--deb" {
            build_deb = true;
            continue;
        }

        if arg == "--rpm" {
            build_rpm = true;
            continue;
        }

        if arg == "--release" {
            set_profile_option(
                &mut profile,
                &mut profile_explicit,
                Some(OsString::from("release")),
            )?;
            continue;
        }

        if arg == "--debug" {
            set_profile_option(
                &mut profile,
                &mut profile_explicit,
                Some(OsString::from("debug")),
            )?;
            continue;
        }

        if arg == "--no-profile" {
            set_profile_option(&mut profile, &mut profile_explicit, None)?;
            continue;
        }

        if arg == "--profile" {
            let value = args.next().ok_or_else(|| {
                TaskError::Usage(String::from(
                    "--profile requires a value; see `cargo xtask package --help`",
                ))
            })?;

            if value.is_empty() {
                return Err(TaskError::Usage(String::from(
                    "--profile requires a non-empty value",
                )));
            }

            set_profile_option(&mut profile, &mut profile_explicit, Some(value))?;
            continue;
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for package command",
            arg.to_string_lossy()
        )));
    }

    if !build_deb && !build_rpm {
        build_deb = true;
        build_rpm = true;
    }

    Ok(PackageOptions {
        build_deb,
        build_rpm,
        profile,
    })
}

fn set_profile_option(
    profile: &mut Option<OsString>,
    explicit: &mut bool,
    value: Option<OsString>,
) -> Result<(), TaskError> {
    if *explicit {
        return Err(TaskError::Usage(String::from(
            "profile specified multiple times; choose at most one of --profile/--release/--debug/--no-profile",
        )));
    }

    *profile = value;
    *explicit = true;
    Ok(())
}

fn execute_package(workspace: &Path, options: PackageOptions) -> Result<(), TaskError> {
    let branding = load_workspace_branding(workspace)?;
    println!("Preparing {}", branding.summary());

    if options.build_deb {
        println!("Building Debian package with cargo deb");
        let mut deb_args = vec![OsString::from("deb"), OsString::from("--locked")];
        if let Some(profile) = &options.profile {
            deb_args.push(OsString::from("--profile"));
            deb_args.push(profile.clone());
        }
        run_cargo_tool(
            workspace,
            deb_args,
            "cargo deb",
            "install the cargo-deb subcommand (cargo install cargo-deb)",
        )?;
    }

    if options.build_rpm {
        println!("Building RPM package with cargo rpm build");
        let mut rpm_args = vec![OsString::from("rpm"), OsString::from("build")];
        if let Some(profile) = &options.profile {
            rpm_args.push(OsString::from("--profile"));
            rpm_args.push(profile.clone());
        }
        run_cargo_tool(
            workspace,
            rpm_args,
            "cargo rpm build",
            "install the cargo-rpm subcommand (cargo install cargo-rpm)",
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        PackageOptions, SbomOptions, TaskError, WorkspaceBranding, package_usage,
        parse_package_args, parse_sbom_args, parse_workspace_branding, sbom_usage, top_level_usage,
    };
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

    #[test]
    fn top_level_usage_mentions_package_command() {
        let usage = top_level_usage();
        assert!(usage.contains("package"));
    }

    #[test]
    fn parse_package_args_accepts_default_configuration() {
        let options = parse_package_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(
            options,
            PackageOptions {
                build_deb: true,
                build_rpm: true,
                profile: Some(OsString::from("release")),
            }
        );
    }

    #[test]
    fn parse_package_args_selects_deb_only() {
        let options = parse_package_args([OsString::from("--deb")]).expect("parse succeeds");
        assert_eq!(
            options,
            PackageOptions {
                build_deb: true,
                build_rpm: false,
                profile: Some(OsString::from("release")),
            }
        );
    }

    #[test]
    fn parse_package_args_selects_rpm_only() {
        let options = parse_package_args([OsString::from("--rpm")]).expect("parse succeeds");
        assert_eq!(
            options,
            PackageOptions {
                build_deb: false,
                build_rpm: true,
                profile: Some(OsString::from("release")),
            }
        );
    }

    #[test]
    fn parse_package_args_accepts_custom_profile() {
        let options = parse_package_args([
            OsString::from("--profile"),
            OsString::from("debug"),
            OsString::from("--deb"),
        ])
        .expect("parse succeeds");
        assert_eq!(
            options,
            PackageOptions {
                build_deb: true,
                build_rpm: false,
                profile: Some(OsString::from("debug")),
            }
        );
    }

    #[test]
    fn parse_package_args_supports_no_profile() {
        let options = parse_package_args([OsString::from("--no-profile")]).expect("parse succeeds");
        assert_eq!(
            options,
            PackageOptions {
                build_deb: true,
                build_rpm: true,
                profile: None,
            }
        );
    }

    #[test]
    fn parse_package_args_rejects_duplicate_profile_flags() {
        let error = parse_package_args([
            OsString::from("--profile"),
            OsString::from("release"),
            OsString::from("--debug"),
        ])
        .unwrap_err();
        assert!(matches!(
            error,
            TaskError::Usage(message)
                if message.contains("profile specified multiple times")
        ));
    }

    #[test]
    fn parse_package_args_requires_profile_value() {
        let error = parse_package_args([OsString::from("--profile")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--profile")));
    }

    #[test]
    fn parse_package_args_reports_help_request() {
        let error = parse_package_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == package_usage()));
    }

    #[test]
    fn parse_package_args_rejects_unknown_argument() {
        let error = parse_package_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("unknown")));
    }

    #[test]
    fn parse_workspace_branding_extracts_fields() {
        let manifest = include_str!("../../Cargo.toml");
        let branding = parse_workspace_branding(manifest).expect("parse succeeds");
        let expected = WorkspaceBranding {
            brand: String::from("oc"),
            upstream_version: String::from("3.4.1"),
            rust_version: String::from("3.4.1-rust"),
            protocol: 32,
            client_bin: String::from("oc-rsync"),
            daemon_bin: String::from("oc-rsyncd"),
            legacy_client_bin: String::from("rsync"),
            legacy_daemon_bin: String::from("rsyncd"),
            daemon_config_dir: String::from("/etc/oc-rsyncd"),
            daemon_config: String::from("/etc/oc-rsyncd/oc-rsyncd.conf"),
            daemon_secrets: String::from("/etc/oc-rsyncd/oc-rsyncd.secrets"),
            source: String::from("https://github.com/oferchen/rsync"),
        };
        assert_eq!(branding, expected);
    }

    #[test]
    fn parse_workspace_branding_reports_missing_tables() {
        let manifest = r#"[workspace]\n[workspace.metadata]\n"#;
        let error = parse_workspace_branding(manifest).unwrap_err();
        assert!(matches!(error, TaskError::Metadata(_)));
    }
}
