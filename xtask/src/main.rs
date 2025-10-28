#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! # Overview
//!
//! The `xtask` utility hosts workspace maintenance commands that are not part of
//! the shipping binaries. In addition to producing a CycloneDX Software Bill of
//! Materials (SBOM) so packaging automation can ship reproducible metadata
//! alongside the Rust `rsync` binaries, the tool now exposes a `preflight`
//! command that validates workspace branding, version alignment, documentation,
//! and packaging assets before CI continues.
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

use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Component, Path, PathBuf};
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
    /// Legacy daemon configuration directory used by upstream-compatible
    /// installations.
    legacy_daemon_config_dir: String,
    /// Legacy daemon configuration file path used by upstream-compatible
    /// installations.
    legacy_daemon_config: String,
    /// Legacy daemon secrets file path used by upstream-compatible
    /// installations.
    legacy_daemon_secrets: String,
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
    parse_workspace_branding_from_value(&value)
}

fn parse_workspace_branding_from_value(value: &Value) -> Result<WorkspaceBranding, TaskError> {
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
        legacy_daemon_config_dir: metadata_str(oc, "legacy_daemon_config_dir")?,
        legacy_daemon_config: metadata_str(oc, "legacy_daemon_config")?,
        legacy_daemon_secrets: metadata_str(oc, "legacy_daemon_secrets")?,
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
        "branding" => {
            let options = parse_branding_args(args)?;
            let workspace = workspace_root()?;
            execute_branding(&workspace, options)
        }
        "docs" => {
            let options = parse_docs_args(args)?;
            let workspace = workspace_root()?;
            execute_docs(&workspace, options)
        }
        "no-binaries" => {
            let options = parse_no_binaries_args(args)?;
            let workspace = workspace_root()?;
            execute_no_binaries(&workspace, options)
        }
        "enforce-limits" => {
            let options = parse_enforce_limits_args(args)?;
            let workspace = workspace_root()?;
            execute_enforce_limits(&workspace, options)
        }
        "no-placeholders" => {
            let options = parse_no_placeholders_args(args)?;
            let workspace = workspace_root()?;
            execute_no_placeholders(&workspace, options)
        }
        "preflight" => {
            let options = parse_preflight_args(args)?;
            let workspace = workspace_root()?;
            execute_preflight(&workspace, options)
        }
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BrandingOptions;

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
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(branding_usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for branding command",
            arg.to_string_lossy()
        )));
    }

    Ok(BrandingOptions)
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

fn execute_branding(workspace: &Path, _options: BrandingOptions) -> Result<(), TaskError> {
    let branding = load_workspace_branding(workspace)?;

    println!("Workspace branding summary:");
    println!("  brand: {}", branding.brand);
    println!("  upstream_version: {}", branding.upstream_version);
    println!("  rust_version: {}", branding.rust_version);
    println!("  protocol: {}", branding.protocol);
    println!("  client_bin: {}", branding.client_bin);
    println!("  daemon_bin: {}", branding.daemon_bin);
    println!("  legacy_client_bin: {}", branding.legacy_client_bin);
    println!("  legacy_daemon_bin: {}", branding.legacy_daemon_bin);
    println!("  daemon_config_dir: {}", branding.daemon_config_dir);
    println!("  daemon_config: {}", branding.daemon_config);
    println!("  daemon_secrets: {}", branding.daemon_secrets);
    println!(
        "  legacy_daemon_config_dir: {}",
        branding.legacy_daemon_config_dir
    );
    println!("  legacy_daemon_config: {}", branding.legacy_daemon_config);
    println!(
        "  legacy_daemon_secrets: {}",
        branding.legacy_daemon_secrets
    );
    println!("  source: {}", branding.source);

    Ok(())
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

fn execute_no_binaries(workspace: &Path, _options: NoBinariesOptions) -> Result<(), TaskError> {
    let tracked_files = list_tracked_files(workspace)?;
    let mut binary_paths = Vec::new();

    for relative in tracked_files {
        let absolute = workspace.join(&relative);
        if is_probably_binary(&absolute)? {
            binary_paths.push(relative);
        }
    }

    if binary_paths.is_empty() {
        println!("No tracked binary files detected.");
        return Ok(());
    }

    binary_paths.sort();
    Err(TaskError::BinaryFiles(binary_paths))
}

fn execute_enforce_limits(
    workspace: &Path,
    options: EnforceLimitsOptions,
) -> Result<(), TaskError> {
    const DEFAULT_MAX_LINES: usize = 600;
    const DEFAULT_WARN_LINES: usize = 400;

    let EnforceLimitsOptions {
        max_lines: cli_max,
        warn_lines: cli_warn,
        config_path,
    } = options;

    let env_max = read_limit_env_var("MAX_RUST_LINES")?;
    let env_warn = read_limit_env_var("WARN_RUST_LINES")?;

    let config_path = resolve_enforce_limits_config_path(workspace, config_path)?;
    let config = if let Some(path) = config_path {
        Some(load_line_limits_config(workspace, &path)?)
    } else {
        None
    };

    let max_lines = cli_max
        .or(env_max)
        .or_else(|| config.as_ref().and_then(|cfg| cfg.default_max_lines))
        .unwrap_or(DEFAULT_MAX_LINES);
    let warn_lines = cli_warn
        .or(env_warn)
        .or_else(|| config.as_ref().and_then(|cfg| cfg.default_warn_lines))
        .unwrap_or(DEFAULT_WARN_LINES);

    if warn_lines > max_lines {
        return Err(validation_error(format!(
            "warn line limit ({warn_lines}) cannot exceed maximum line limit ({max_lines})"
        )));
    }

    let rust_files = collect_rust_sources(workspace)?;
    if rust_files.is_empty() {
        eprintln!("No Rust sources found.");
        return Ok(());
    }

    let mut failure_detected = false;
    let mut warned = false;

    for path in rust_files {
        let mut file_max = max_lines;
        let mut file_warn = warn_lines;

        if let Some(config) = &config {
            let relative = path
                .strip_prefix(workspace)
                .map_err(|_| {
                    validation_error(format!(
                        "failed to compute path relative to workspace for {}",
                        path.display()
                    ))
                })?
                .to_path_buf();

            if let Some(override_limits) = config.override_for(&relative) {
                if let Some(max_override) = override_limits.max_lines {
                    file_max = max_override;
                }
                if let Some(warn_override) = override_limits.warn_lines {
                    file_warn = warn_override;
                }
            }

            if file_warn > file_max {
                return Err(validation_error(format!(
                    "override for {} sets warn_lines ({}) above max_lines ({})",
                    relative.display(),
                    file_warn,
                    file_max
                )));
            }
        }

        let line_count = count_file_lines(&path)?;
        if line_count > file_max {
            eprintln!(
                "::error file={}::Rust source has {} lines (limit {})",
                path.display(),
                line_count,
                file_max
            );
            failure_detected = true;
            continue;
        }

        if line_count > file_warn {
            eprintln!(
                "::warning file={}::Rust source has {} lines (target {})",
                path.display(),
                line_count,
                file_warn
            );
            warned = true;
        }
    }

    if failure_detected {
        return Err(validation_error(
            "Rust source files exceed the enforced maximum line count.",
        ));
    }

    if warned {
        eprintln!("Rust source files exceed target length but remain under the enforced limit.");
    }

    Ok(())
}

fn execute_no_placeholders(
    workspace: &Path,
    _options: NoPlaceholdersOptions,
) -> Result<(), TaskError> {
    let mut violations_present = false;
    let rust_files = list_rust_sources_via_git(workspace)?;

    for relative in rust_files {
        let absolute = workspace.join(&relative);
        let findings = scan_rust_file_for_placeholders(&absolute)?;
        if findings.is_empty() {
            continue;
        }

        violations_present = true;
        for finding in findings {
            eprintln!(
                "{}:{}:{}",
                relative.display(),
                finding.line,
                finding.snippet
            );
        }
    }

    if violations_present {
        return Err(validation_error(
            "placeholder markers detected in Rust sources; remove todo/unimplemented/fixme/xxx references",
        ));
    }

    Ok(())
}

fn execute_preflight(workspace: &Path, _options: PreflightOptions) -> Result<(), TaskError> {
    let manifest_text = read_workspace_manifest(workspace)?;
    let manifest_value = manifest_text.parse::<Value>().map_err(|error| {
        TaskError::Metadata(format!("failed to parse workspace manifest: {error}"))
    })?;
    let branding = parse_workspace_branding_from_value(&manifest_value)?;

    validate_branding(&branding)?;
    validate_packaging_assets(workspace, &branding)?;
    validate_package_versions(workspace, &branding)?;
    validate_workspace_package_rust_version(&manifest_value)?;
    validate_documentation(workspace, &branding)?;

    println!(
        "Preflight checks passed: branding, version, packaging metadata, documentation, and toolchain requirements validated."
    );

    Ok(())
}

fn validate_branding(branding: &WorkspaceBranding) -> Result<(), TaskError> {
    ensure(
        branding.brand == "oc",
        format!("workspace brand must be 'oc', found {:?}", branding.brand),
    )?;
    ensure(
        branding.upstream_version == "3.4.1",
        format!(
            "upstream_version must remain aligned with rsync 3.4.1; found {:?}",
            branding.upstream_version
        ),
    )?;
    ensure(
        branding.rust_version.ends_with("-rust"),
        format!(
            "Rust-branded version should end with '-rust'; found {:?}",
            branding.rust_version
        ),
    )?;
    ensure(
        branding.protocol == 32,
        format!("Supported protocol must be 32; found {}", branding.protocol),
    )?;
    ensure(
        branding.client_bin.starts_with("oc-"),
        format!(
            "client_bin must start with 'oc-'; found {:?}",
            branding.client_bin
        ),
    )?;
    ensure(
        branding.daemon_bin.starts_with("oc-"),
        format!(
            "daemon_bin must start with 'oc-'; found {:?}",
            branding.daemon_bin
        ),
    )?;

    let config_dir = Path::new(&branding.daemon_config_dir);
    ensure(
        config_dir.is_absolute(),
        format!(
            "daemon_config_dir must be an absolute path; found {}",
            branding.daemon_config_dir
        ),
    )?;

    let config_path = Path::new(&branding.daemon_config);
    let secrets_path = Path::new(&branding.daemon_secrets);
    ensure(
        config_path.is_absolute(),
        format!(
            "daemon_config must be an absolute path; found {}",
            branding.daemon_config
        ),
    )?;
    ensure(
        secrets_path.is_absolute(),
        format!(
            "daemon_secrets must be an absolute path; found {}",
            branding.daemon_secrets
        ),
    )?;

    ensure(
        config_path.parent() == Some(config_dir),
        format!(
            "daemon_config {} must reside within configured directory {}",
            branding.daemon_config, branding.daemon_config_dir
        ),
    )?;
    ensure(
        secrets_path.parent() == Some(config_dir),
        format!(
            "daemon_secrets {} must reside within configured directory {}",
            branding.daemon_secrets, branding.daemon_config_dir
        ),
    )?;

    ensure(
        config_path.file_name() != secrets_path.file_name(),
        "daemon configuration and secrets paths must not collide",
    )?;

    Ok(())
}

fn validate_packaging_assets(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> Result<(), TaskError> {
    let packaging_root = workspace.join("packaging").join("etc").join("oc-rsyncd");
    let config_name = Path::new(&branding.daemon_config)
        .file_name()
        .ok_or_else(|| validation_error("daemon_config must include a file name"))?;
    let secrets_name = Path::new(&branding.daemon_secrets)
        .file_name()
        .ok_or_else(|| validation_error("daemon_secrets must include a file name"))?;

    let assets = [
        (config_name, "daemon_config"),
        (secrets_name, "daemon_secrets"),
    ];

    for (name, label) in assets {
        let candidate = packaging_root.join(name);
        ensure(
            candidate.exists(),
            format!(
                "packaging assets missing for {} (expected {})",
                label,
                candidate.display()
            ),
        )?;
    }

    let systemd_unit = workspace
        .join("packaging")
        .join("systemd")
        .join("oc-rsyncd.service");
    let unit_contents = fs::read_to_string(&systemd_unit).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to read {}: {}", systemd_unit.display(), error),
        ))
    })?;

    let unit_snippets = [
        branding.daemon_bin.as_str(),
        branding.daemon_config.as_str(),
        branding.daemon_secrets.as_str(),
        "Description=oc-rsyncd",
        "Alias=rsyncd.service",
        "OC_RSYNC_CONFIG",
        "OC_RSYNC_SECRETS",
        "RSYNCD_CONFIG",
        "RSYNCD_SECRETS",
    ];

    for snippet in unit_snippets {
        ensure(
            unit_contents.contains(snippet),
            format!(
                "systemd unit {} missing required snippet '{}': update packaging/systemd/oc-rsyncd.service",
                systemd_unit.display(),
                snippet
            ),
        )?;
    }

    let env_file = workspace
        .join("packaging")
        .join("default")
        .join("oc-rsyncd");
    let env_contents = fs::read_to_string(&env_file).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to read {}: {}", env_file.display(), error),
        ))
    })?;

    let env_snippets = [
        "OC_RSYNC_CONFIG",
        "RSYNCD_CONFIG",
        "OC_RSYNC_SECRETS",
        "RSYNCD_SECRETS",
    ];

    for snippet in env_snippets {
        ensure(
            env_contents.contains(snippet),
            format!(
                "environment defaults {} missing '{}': update packaging/default/oc-rsyncd",
                env_file.display(),
                snippet
            ),
        )?;
    }

    Ok(())
}

fn validate_package_versions(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> Result<(), TaskError> {
    let metadata = cargo_metadata_json(workspace)?;
    let packages = metadata
        .get("packages")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| validation_error("cargo metadata output missing packages array"))?;

    let mut versions = HashMap::new();
    for package in packages {
        let Some(name) = package.get("name").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(version) = package.get("version").and_then(JsonValue::as_str) else {
            continue;
        };
        versions.insert(name.to_string(), version.to_string());
    }

    for crate_name in ["oc-rsync-bin", "oc-rsyncd-bin"] {
        let version = versions.get(crate_name).ok_or_else(|| {
            validation_error(format!("crate {crate_name} missing from cargo metadata"))
        })?;
        ensure(
            version == &branding.rust_version,
            format!(
                "crate {crate_name} version {version} does not match {}",
                branding.rust_version
            ),
        )?;
    }

    Ok(())
}

fn validate_workspace_package_rust_version(manifest: &Value) -> Result<(), TaskError> {
    let workspace = manifest
        .get("workspace")
        .and_then(Value::as_table)
        .ok_or_else(|| validation_error("missing [workspace] table in Cargo.toml"))?;
    let package = workspace
        .get("package")
        .and_then(Value::as_table)
        .ok_or_else(|| validation_error("missing [workspace.package] table in Cargo.toml"))?;
    let rust_version = package
        .get("rust-version")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            validation_error("workspace.package.rust-version missing from Cargo.toml")
        })?;

    ensure(
        rust_version == "1.87",
        format!(
            "workspace.package.rust-version must match CI toolchain 1.87; found {:?}",
            rust_version
        ),
    )
}

fn validate_documentation(workspace: &Path, branding: &WorkspaceBranding) -> Result<(), TaskError> {
    struct DocumentationCheck<'a> {
        relative_path: &'a str,
        required_snippets: Vec<&'a str>,
    }

    let checks = [
        DocumentationCheck {
            relative_path: "README.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/production_scope_p1.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config_dir.as_str(),
                branding.daemon_config.as_str(),
                branding.daemon_secrets.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/differences.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/gaps.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/COMPARE.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config.as_str(),
            ],
        },
    ];

    for check in checks {
        let path = workspace.join(check.relative_path);
        let contents = fs::read_to_string(&path).map_err(|error| {
            TaskError::Io(io::Error::new(
                error.kind(),
                format!("failed to read {}: {error}", path.display()),
            ))
        })?;

        let missing: Vec<&str> = check
            .required_snippets
            .iter()
            .copied()
            .filter(|snippet| !snippet.is_empty() && !contents.contains(snippet))
            .collect();

        ensure(
            missing.is_empty(),
            format!(
                "{} missing required documentation snippets: {}",
                check.relative_path,
                missing
                    .iter()
                    .map(|snippet| format!("'{}'", snippet))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )?;
    }

    Ok(())
}

fn cargo_metadata_json(workspace: &Path) -> Result<JsonValue, TaskError> {
    let output = Command::new("cargo")
        .current_dir(workspace)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|error| map_command_error(error, "cargo metadata", "ensure Cargo is installed"))?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("cargo metadata"),
            status: output.status,
        });
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        TaskError::Metadata(format!("failed to parse cargo metadata output: {error}"))
    })
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<(), TaskError> {
    if condition {
        Ok(())
    } else {
        Err(validation_error(message))
    }
}

fn validation_error(message: impl Into<String>) -> TaskError {
    TaskError::Validation(message.into())
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
        return Err(TaskError::Io(io::Error::other(
            "failed to locate workspace root",
        )));
    }
    Ok(path)
}

fn top_level_usage() -> String {
    String::from(
        "Usage: cargo xtask <command>\n\nCommands:\n  branding          Display workspace branding metadata\n  docs              Build API documentation and run doctests\n  enforce-limits    Enforce Rust source line count caps\n  no-binaries       Ensure no tracked binary artifacts are committed\n  no-placeholders   Scan Rust sources for placeholder markers\n  preflight         Validate branding, packaging, and documentation metadata\n  package           Build Debian and RPM packages (requires cargo-deb and cargo-rpm)\n  sbom              Generate a CycloneDX SBOM (requires cargo-cyclonedx)\n  help              Show this help message\n\nRun `cargo xtask <command> --help` for details about a specific command.",
    )
}

fn docs_usage() -> String {
    String::from(
        "Usage: cargo xtask docs [--open]\n\nOptions:\n  --open          Open the generated documentation in a browser after building\n  -h, --help      Show this help message",
    )
}

fn branding_usage() -> String {
    String::from(
        "Usage: cargo xtask branding\n\nOptions:\n  -h, --help      Show this help message",
    )
}

fn no_binaries_usage() -> String {
    String::from(
        "Usage: cargo xtask no-binaries\n\nOptions:\n  -h, --help      Show this help message",
    )
}

fn enforce_limits_usage() -> String {
    String::from(
        "Usage: cargo xtask enforce-limits [OPTIONS]\n\nOptions:\n  --max-lines NUM   Override the maximum allowed lines per Rust source file\n  --warn-lines NUM  Override the warning threshold for Rust source files\n  --config PATH     Load overrides from the given TOML configuration\n  -h, --help        Show this help message",
    )
}

fn no_placeholders_usage() -> String {
    String::from(
        "Usage: cargo xtask no-placeholders\n\nOptions:\n  -h, --help      Show this help message",
    )
}

fn preflight_usage() -> String {
    String::from(
        "Usage: cargo xtask preflight\n\nOptions:\n  -h, --help      Show this help message",
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
    Validation(String),
    BinaryFiles(Vec<PathBuf>),
    CommandFailed { program: String, status: ExitStatus },
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Usage(message)
            | TaskError::Help(message)
            | TaskError::Validation(message) => f.write_str(message),
            TaskError::Io(error) => write!(f, "{error}"),
            TaskError::ToolMissing(message) => f.write_str(message),
            TaskError::Metadata(message) => f.write_str(message),
            TaskError::BinaryFiles(paths) => {
                writeln!(f, "binary files detected in repository:")?;
                for path in paths {
                    writeln!(f, "  {}", path.display())?;
                }
                Ok(())
            }
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

fn list_tracked_files(workspace: &Path) -> Result<Vec<PathBuf>, TaskError> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "git ls-files",
                "install git and ensure it is available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("git ls-files"),
            status: output.status,
        });
    }

    let mut files = Vec::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            files.push(PathBuf::from(OsString::from_vec(entry.to_vec())));
        }

        #[cfg(not(unix))]
        {
            let path = String::from_utf8(entry.to_vec()).map_err(|_| {
                TaskError::Metadata(String::from(
                    "git reported a non-UTF-8 path; binary audit requires UTF-8 file names on this platform",
                ))
            })?;
            files.push(PathBuf::from(path));
        }
    }

    Ok(files)
}

fn read_limit_env_var(name: &str) -> Result<Option<usize>, TaskError> {
    match env::var(name) {
        Ok(value) => {
            if value.is_empty() {
                return Err(validation_error(format!(
                    "{name} must be a positive integer, found an empty value"
                )));
            }

            let parsed = parse_positive_usize_from_env(name, &value)?;
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(validation_error(format!(
            "{name} must contain a UTF-8 encoded positive integer"
        ))),
    }
}

fn parse_positive_usize_from_env(name: &str, value: &str) -> Result<usize, TaskError> {
    let parsed = value.parse::<usize>().map_err(|_| {
        validation_error(format!(
            "{name} must be a positive integer, found '{value}'"
        ))
    })?;

    if parsed == 0 {
        return Err(validation_error(format!(
            "{name} must be greater than zero, found '{value}'"
        )));
    }

    Ok(parsed)
}

fn parse_positive_usize_arg(value: &OsString, flag: &str) -> Result<usize, TaskError> {
    let text = value.to_str().ok_or_else(|| {
        TaskError::Usage(format!("{flag} requires a UTF-8 positive integer value"))
    })?;

    let parsed = text.parse::<usize>().map_err(|_| {
        TaskError::Usage(format!(
            "{flag} requires a positive integer value, found '{text}'"
        ))
    })?;

    if parsed == 0 {
        return Err(TaskError::Usage(format!(
            "{flag} requires a positive integer value, found '{text}'"
        )));
    }

    Ok(parsed)
}

fn collect_rust_sources(root: &Path) -> Result<Vec<PathBuf>, TaskError> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;

            if metadata.is_dir() {
                if should_skip_directory(&path) {
                    continue;
                }
                stack.push(path);
                continue;
            }

            if metadata.is_file() {
                if let Some(ext) = path.extension() {
                    if ext.eq_ignore_ascii_case("rs") {
                        files.push(path);
                    }
                }
            }
        }
    }

    files.sort();
    Ok(files)
}

fn should_skip_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("target") | Some(".git")
    )
}

fn count_file_lines(path: &Path) -> Result<usize, TaskError> {
    let file = fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);
    let mut buffer = String::new();
    let mut count = 0usize;

    loop {
        buffer.clear();
        let read = reader.read_line(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += 1;
    }

    Ok(count)
}

#[derive(Debug, Default)]
struct LineLimitsConfig {
    default_max_lines: Option<usize>,
    default_warn_lines: Option<usize>,
    overrides: HashMap<PathBuf, FileLineLimit>,
}

impl LineLimitsConfig {
    fn override_for(&self, relative: &Path) -> Option<&FileLineLimit> {
        self.overrides.get(relative)
    }
}

#[derive(Clone, Copy, Debug)]
struct FileLineLimit {
    max_lines: Option<usize>,
    warn_lines: Option<usize>,
}

fn resolve_enforce_limits_config_path(
    workspace: &Path,
    explicit: Option<PathBuf>,
) -> Result<Option<PathBuf>, TaskError> {
    if let Some(path) = explicit {
        let resolved = if path.is_absolute() {
            path
        } else {
            workspace.join(path)
        };

        if !resolved.exists() {
            return Err(validation_error(format!(
                "line limit configuration {} does not exist",
                resolved.display()
            )));
        }

        if !resolved.is_file() {
            return Err(validation_error(format!(
                "line limit configuration {} is not a regular file",
                resolved.display()
            )));
        }

        return Ok(Some(resolved));
    }

    let default = workspace.join("tools/line_limits.toml");
    if default.exists() {
        if !default.is_file() {
            return Err(validation_error(format!(
                "expected {} to be a regular file for enforce-limits configuration",
                default.display()
            )));
        }
        return Ok(Some(default));
    }

    Ok(None)
}

fn load_line_limits_config(workspace: &Path, path: &Path) -> Result<LineLimitsConfig, TaskError> {
    let contents = fs::read_to_string(path).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!(
                "failed to read enforce-limits configuration at {}: {error}",
                path.display()
            ),
        ))
    })?;

    let config = parse_line_limits_config(&contents, path)?;
    validate_line_limit_overrides(workspace, path, &config)?;
    Ok(config)
}

fn parse_line_limits_config(contents: &str, origin: &Path) -> Result<LineLimitsConfig, TaskError> {
    let value = contents.parse::<Value>().map_err(|error| {
        validation_error(format!(
            "failed to parse {} as TOML: {error}",
            origin.display()
        ))
    })?;

    let table = value.as_table().ok_or_else(|| {
        validation_error(format!(
            "line limit configuration {} must be a TOML table",
            origin.display()
        ))
    })?;

    let mut config = LineLimitsConfig::default();

    if let Some(default_max) = table.get("default_max_lines") {
        config.default_max_lines = Some(parse_positive_usize_value(
            default_max,
            "default_max_lines",
            origin,
        )?);
    }

    if let Some(default_warn) = table.get("default_warn_lines") {
        config.default_warn_lines = Some(parse_positive_usize_value(
            default_warn,
            "default_warn_lines",
            origin,
        )?);
    }

    if let (Some(warn), Some(max)) = (config.default_warn_lines, config.default_max_lines) {
        if warn > max {
            return Err(validation_error(format!(
                "default warn_lines ({warn}) cannot exceed default max_lines ({max}) in {}",
                origin.display()
            )));
        }
    }

    if let Some(overrides) = table.get("overrides") {
        let entries = overrides.as_array().ok_or_else(|| {
            validation_error(format!(
                "'overrides' in {} must be an array",
                origin.display()
            ))
        })?;

        for (index, entry) in entries.iter().enumerate() {
            let entry_table = entry.as_table().ok_or_else(|| {
                validation_error(format!(
                    "override #{index} in {} must be a table",
                    origin.display()
                ))
            })?;

            let path_value = entry_table
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    validation_error(format!(
                        "override #{index} in {} must include a string 'path' field",
                        origin.display()
                    ))
                })?;

            let override_path = PathBuf::from(path_value);
            if override_path.as_os_str().is_empty() {
                return Err(validation_error(format!(
                    "override #{index} in {} has an empty path",
                    origin.display()
                )));
            }

            if override_path.is_absolute() {
                return Err(validation_error(format!(
                    "override path {} in {} must be relative",
                    override_path.display(),
                    origin.display()
                )));
            }

            if override_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(validation_error(format!(
                    "override path {} in {} may not contain parent directory components",
                    override_path.display(),
                    origin.display()
                )));
            }

            if config.overrides.contains_key(&override_path) {
                return Err(validation_error(format!(
                    "duplicate override for {} in {}",
                    override_path.display(),
                    origin.display()
                )));
            }

            let max_key = format!("overrides[{index}].max_lines");
            let warn_key = format!("overrides[{index}].warn_lines");

            let max_lines = entry_table
                .get("max_lines")
                .map(|value| parse_positive_usize_value(value, &max_key, origin))
                .transpose()?;
            let warn_lines = entry_table
                .get("warn_lines")
                .map(|value| parse_positive_usize_value(value, &warn_key, origin))
                .transpose()?;

            if let (Some(warn), Some(max)) = (warn_lines, max_lines) {
                if warn > max {
                    return Err(validation_error(format!(
                        "override for {} in {} has warn_lines ({warn}) exceeding max_lines ({max})",
                        override_path.display(),
                        origin.display()
                    )));
                }
            }

            config.overrides.insert(
                override_path,
                FileLineLimit {
                    max_lines,
                    warn_lines,
                },
            );
        }
    }

    Ok(config)
}

fn validate_line_limit_overrides(
    workspace: &Path,
    origin: &Path,
    config: &LineLimitsConfig,
) -> Result<(), TaskError> {
    for relative in config.overrides.keys() {
        let candidate = workspace.join(relative);
        if !candidate.exists() {
            return Err(validation_error(format!(
                "override path {} in {} does not exist",
                relative.display(),
                origin.display()
            )));
        }

        if !candidate.is_file() {
            return Err(validation_error(format!(
                "override path {} in {} is not a regular file",
                relative.display(),
                origin.display()
            )));
        }
    }

    Ok(())
}

fn parse_positive_usize_value(
    value: &Value,
    field: &str,
    origin: &Path,
) -> Result<usize, TaskError> {
    let integer = value.as_integer().ok_or_else(|| {
        validation_error(format!(
            "{field} in {} must be an integer",
            origin.display()
        ))
    })?;

    if integer <= 0 {
        return Err(validation_error(format!(
            "{field} in {} must be a positive integer",
            origin.display()
        )));
    }

    usize::try_from(integer).map_err(|_| {
        validation_error(format!(
            "{field} in {} exceeds supported range",
            origin.display()
        ))
    })
}

fn list_rust_sources_via_git(workspace: &Path) -> Result<Vec<PathBuf>, TaskError> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "git ls-files",
                "install git and ensure it is available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("git ls-files"),
            status: output.status,
        });
    }

    let mut files = Vec::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            files.push(PathBuf::from(OsString::from_vec(entry.to_vec())));
        }

        #[cfg(not(unix))]
        {
            let path = String::from_utf8(entry.to_vec()).map_err(|_| {
                TaskError::Metadata(String::from(
                    "git reported a non-UTF-8 path; placeholder scanning requires UTF-8 file names on this platform",
                ))
            })?;
            files.push(PathBuf::from(path));
        }
    }

    files.sort();
    files.dedup();
    Ok(files)
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PlaceholderFinding {
    line: usize,
    snippet: String,
}

fn scan_rust_file_for_placeholders(path: &Path) -> Result<Vec<PlaceholderFinding>, TaskError> {
    let file = fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);
    let mut buffer = String::new();
    let mut findings = Vec::new();
    let mut line_number = 0usize;

    loop {
        buffer.clear();
        let read = reader.read_line(&mut buffer)?;
        if read == 0 {
            break;
        }

        line_number += 1;
        if line_number == 1 {
            continue;
        }

        let line = buffer.trim_end_matches(['\r', '\n']);
        if contains_placeholder(line) {
            findings.push(PlaceholderFinding {
                line: line_number,
                snippet: line.to_string(),
            });
        }
    }

    Ok(findings)
}

fn contains_placeholder(line: &str) -> bool {
    if line.contains("todo!") || line.contains("unimplemented!") {
        return true;
    }

    let lower = line.to_ascii_lowercase();
    if contains_standalone_word(&lower, "fixme") || contains_standalone_word(&lower, "xxx") {
        return true;
    }

    if line.contains("panic!")
        && (contains_standalone_word(&lower, "todo")
            || contains_standalone_word(&lower, "fixme")
            || contains_standalone_word(&lower, "xxx")
            || contains_standalone_word(&lower, "unimplemented"))
    {
        return true;
    }

    false
}

fn contains_standalone_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();

    if needle_bytes.is_empty() || bytes.len() < needle_bytes.len() {
        return false;
    }

    let mut index = 0usize;
    while let Some(position) = haystack[index..].find(needle) {
        let absolute = index + position;
        let before_ok = absolute == 0 || !is_identifier_byte(bytes[absolute.saturating_sub(1)]);
        let after_index = absolute + needle_bytes.len();
        let after_ok = after_index >= bytes.len() || !is_identifier_byte(bytes[after_index]);

        if before_ok && after_ok {
            return true;
        }

        index = absolute + 1;
    }

    false
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn is_probably_binary(path: &Path) -> Result<bool, TaskError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Ok(false);
    }

    let mut file = fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let mut printable = 0usize;
    let mut control = 0usize;
    let mut inspected = 0usize;

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        inspected += read;

        for &byte in &buffer[..read] {
            match byte {
                0 => return Ok(true),
                0x07 | 0x08 | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => printable += 1,
                0x20..=0x7E => printable += 1,
                _ if byte >= 0x80 => printable += 1,
                _ => control += 1,
            }
        }

        if control > printable {
            return Ok(true);
        }

        if inspected >= buffer.len() {
            break;
        }
    }

    Ok(false)
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
        BrandingOptions, DocsOptions, EnforceLimitsOptions, NoBinariesOptions,
        NoPlaceholdersOptions, PackageOptions, PreflightOptions, SbomOptions, TaskError,
        WorkspaceBranding, branding_usage, docs_usage, enforce_limits_usage, no_binaries_usage,
        no_placeholders_usage, package_usage, parse_branding_args, parse_docs_args,
        parse_enforce_limits_args, parse_line_limits_config, parse_no_binaries_args,
        parse_no_placeholders_args, parse_package_args, parse_preflight_args, parse_sbom_args,
        parse_workspace_branding, preflight_usage, sbom_usage, scan_rust_file_for_placeholders,
        top_level_usage,
    };
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn parse_branding_args_accepts_default_configuration() {
        let options = parse_branding_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, BrandingOptions);
    }

    #[test]
    fn parse_branding_args_reports_help_request() {
        let error = parse_branding_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == branding_usage()));
    }

    #[test]
    fn parse_branding_args_rejects_unknown_argument() {
        let error = parse_branding_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }

    #[test]
    fn top_level_usage_mentions_branding_command() {
        let usage = top_level_usage();
        assert!(usage.contains("branding"));
    }

    #[test]
    fn parse_docs_args_accepts_default_configuration() {
        let options = parse_docs_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, DocsOptions { open: false });
    }

    #[test]
    fn parse_docs_args_enables_open_flag() {
        let options = parse_docs_args([OsString::from("--open")]).expect("parse succeeds");
        assert_eq!(options, DocsOptions { open: true });
    }

    #[test]
    fn parse_docs_args_rejects_duplicate_open_flag() {
        let error =
            parse_docs_args([OsString::from("--open"), OsString::from("--open")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--open")));
    }

    #[test]
    fn parse_docs_args_reports_help_request() {
        let error = parse_docs_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == docs_usage()));
    }

    #[test]
    fn parse_docs_args_rejects_unknown_argument() {
        let error = parse_docs_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }

    #[test]
    fn top_level_usage_mentions_docs_command() {
        let usage = top_level_usage();
        assert!(usage.contains("docs"));
    }

    #[test]
    fn parse_no_binaries_args_accepts_default_configuration() {
        let options = parse_no_binaries_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, NoBinariesOptions);
    }

    #[test]
    fn parse_no_binaries_args_reports_help_request() {
        let error = parse_no_binaries_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == no_binaries_usage()));
    }

    #[test]
    fn parse_no_binaries_args_rejects_unknown_argument() {
        let error = parse_no_binaries_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }

    #[test]
    fn top_level_usage_mentions_no_binaries_command() {
        let usage = top_level_usage();
        assert!(usage.contains("no-binaries"));
    }

    #[test]
    fn parse_enforce_limits_args_accepts_default_configuration() {
        let options = parse_enforce_limits_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, EnforceLimitsOptions::default());
    }

    #[test]
    fn parse_enforce_limits_args_accepts_custom_limits() {
        let options = parse_enforce_limits_args([
            OsString::from("--max-lines"),
            OsString::from("700"),
            OsString::from("--warn-lines"),
            OsString::from("500"),
        ])
        .expect("parse succeeds");
        assert_eq!(
            options,
            EnforceLimitsOptions {
                max_lines: Some(700),
                warn_lines: Some(500),
                config_path: None,
            }
        );
    }

    #[test]
    fn parse_enforce_limits_args_accepts_config_path() {
        let options = parse_enforce_limits_args([
            OsString::from("--config"),
            OsString::from("tools/line_limits.toml"),
        ])
        .expect("parse succeeds");
        assert_eq!(
            options,
            EnforceLimitsOptions {
                max_lines: None,
                warn_lines: None,
                config_path: Some(PathBuf::from("tools/line_limits.toml")),
            }
        );
    }

    #[test]
    fn parse_line_limits_config_supports_overrides() {
        let config = parse_line_limits_config(
            r#"
default_max_lines = 1200
default_warn_lines = 1000

[[overrides]]
path = "src/lib.rs"
max_lines = 1500
warn_lines = 1400

[[overrides]]
path = "src/bin/main.rs"
warn_lines = 650
"#,
            Path::new("line_limits.toml"),
        )
        .expect("parse succeeds");

        assert_eq!(config.default_max_lines, Some(1200));
        assert_eq!(config.default_warn_lines, Some(1000));

        let primary = config
            .override_for(Path::new("src/lib.rs"))
            .expect("override present");
        assert_eq!(primary.max_lines, Some(1500));
        assert_eq!(primary.warn_lines, Some(1400));

        let secondary = config
            .override_for(Path::new("src/bin/main.rs"))
            .expect("override present");
        assert_eq!(secondary.max_lines, None);
        assert_eq!(secondary.warn_lines, Some(650));
    }

    #[test]
    fn parse_line_limits_config_rejects_parent_directories() {
        let error = parse_line_limits_config(
            r#"
[[overrides]]
path = "../src/lib.rs"
max_lines = 900
"#,
            Path::new("line_limits.toml"),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("parent directory")
        ));
    }

    #[test]
    fn parse_enforce_limits_args_rejects_invalid_values() {
        let error = parse_enforce_limits_args([OsString::from("--max-lines"), OsString::from("0")])
            .unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--max-lines")));
    }

    #[test]
    fn parse_enforce_limits_args_reports_help_request() {
        let error = parse_enforce_limits_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == enforce_limits_usage()));
    }

    #[test]
    fn parse_enforce_limits_args_rejects_warn_exceeding_maximum() {
        let error = parse_enforce_limits_args([
            OsString::from("--warn-lines"),
            OsString::from("800"),
            OsString::from("--max-lines"),
            OsString::from("700"),
        ])
        .unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("warn line limit")));
    }

    #[test]
    fn top_level_usage_mentions_enforce_limits_command() {
        let usage = top_level_usage();
        assert!(usage.contains("enforce-limits"));
    }

    #[test]
    fn parse_no_placeholders_args_accepts_default_configuration() {
        let options = parse_no_placeholders_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, NoPlaceholdersOptions);
    }

    #[test]
    fn parse_no_placeholders_args_reports_help_request() {
        let error = parse_no_placeholders_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == no_placeholders_usage()));
    }

    #[test]
    fn parse_no_placeholders_args_rejects_unknown_argument() {
        let error = parse_no_placeholders_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }

    #[test]
    fn top_level_usage_mentions_no_placeholders_command() {
        let usage = top_level_usage();
        assert!(usage.contains("no-placeholders"));
    }

    #[test]
    fn parse_preflight_args_accepts_default_configuration() {
        let options = parse_preflight_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, PreflightOptions);
    }

    #[test]
    fn parse_preflight_args_reports_help_request() {
        let error = parse_preflight_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == preflight_usage()));
    }

    #[test]
    fn parse_preflight_args_rejects_unknown_argument() {
        let error = parse_preflight_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }

    #[test]
    fn top_level_usage_mentions_preflight_command() {
        let usage = top_level_usage();
        assert!(usage.contains("preflight"));
    }

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
            legacy_daemon_config_dir: String::from("/etc"),
            legacy_daemon_config: String::from("/etc/rsyncd.conf"),
            legacy_daemon_secrets: String::from("/etc/rsyncd.secrets"),
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

    #[test]
    fn scan_rust_file_for_placeholders_detects_todo_macro() {
        let path = unique_temp_path("todo_macro");
        fs::write(&path, "fn example() {\n    todo!();\n}\n").expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
        assert!(findings[0].snippet.contains("todo!"));
    }

    #[test]
    fn scan_rust_file_for_placeholders_detects_fixme_comment() {
        let path = unique_temp_path("fixme_comment");
        fs::write(&path, "// header\n// FIXME: implement\nfn ready() {}\n").expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
        assert!(findings[0].snippet.to_ascii_lowercase().contains("fixme"));
    }

    #[test]
    fn scan_rust_file_for_placeholders_ignores_first_line() {
        let path = unique_temp_path("first_line_ignored");
        fs::write(&path, "// TODO: license\nfn ok() {}\n").expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert!(findings.is_empty());
    }

    #[test]
    fn is_probably_binary_flags_null_byte_payloads() {
        let path = unique_temp_path("binary");
        fs::write(&path, b"text\0binary").expect("write sample");
        let is_binary = super::is_probably_binary(&path).expect("analysis succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert!(is_binary);
    }

    #[test]
    fn is_probably_binary_accepts_utf8_text() {
        let path = unique_temp_path("text");
        fs::write(&path, "Rust makes systems programming enjoyable!\n").expect("write sample");
        let is_binary = super::is_probably_binary(&path).expect("analysis succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert!(!is_binary);
    }

    fn unique_temp_path(suffix: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("oc_rsync_xtask_{now}_{suffix}"))
    }
}
