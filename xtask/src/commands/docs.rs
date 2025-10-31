use crate::error::{TaskError, TaskResult};
use crate::util::{is_help_flag, run_cargo_tool};
use crate::workspace::{WorkspaceBranding, load_workspace_branding};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Options accepted by the `docs` command.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocsOptions {
    /// Whether to open the generated documentation in a browser.
    pub open: bool,
    /// Whether to validate documentation snippets against workspace branding metadata.
    pub validate: bool,
}

/// Parses CLI arguments for the `docs` command.
pub fn parse_args<I>(args: I) -> TaskResult<DocsOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = DocsOptions::default();

    for arg in args {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
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

        if arg == "--validate" {
            if options.validate {
                return Err(TaskError::Usage(String::from(
                    "--validate specified multiple times",
                )));
            }
            options.validate = true;
            continue;
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for docs command",
            arg.to_string_lossy()
        )));
    }

    Ok(options)
}

/// Executes the `docs` command.
pub fn execute(workspace: &Path, options: DocsOptions) -> TaskResult<()> {
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
    )?;

    if options.validate {
        println!("Validating documentation branding");
        validate_documents(workspace)?;
    }

    Ok(())
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask docs [--open] [--validate]\n\nOptions:\n  --open          Open documentation after building\n  --validate     Validate branding references in Markdown documents\n  -h, --help      Show this help message",
    )
}

fn validate_documents(workspace: &Path) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    let docs_dir = workspace.join("docs");
    let daemon_config = branding.daemon_config.display().to_string();
    let daemon_secrets = branding.daemon_secrets.display().to_string();

    let checks = [
        DocumentCheck::new(
            workspace.join("README.md"),
            [
                (
                    branding.rust_version.as_str(),
                    "Rust-branded version string",
                ),
                (branding.client_bin.as_str(), "client binary name"),
                (branding.daemon_bin.as_str(), "daemon binary name"),
                (branding.source.as_str(), "source repository URL"),
            ],
        ),
        DocumentCheck::new(
            docs_dir.join("production_scope_p1.md"),
            [
                (branding.client_bin.as_str(), "client binary name"),
                (branding.daemon_bin.as_str(), "daemon binary name"),
                (daemon_config.as_str(), "daemon configuration path"),
                (daemon_secrets.as_str(), "daemon secrets path"),
            ],
        ),
        DocumentCheck::new(
            docs_dir.join("feature_matrix.md"),
            [(branding.client_bin.as_str(), "client binary name")],
        ),
        DocumentCheck::new(
            docs_dir.join("differences.md"),
            [(branding.daemon_bin.as_str(), "daemon binary name")],
        ),
        DocumentCheck::new(
            docs_dir.join("gaps.md"),
            [
                (branding.client_bin.as_str(), "client binary name"),
                (branding.daemon_bin.as_str(), "daemon binary name"),
            ],
        ),
        DocumentCheck::new(
            docs_dir.join("resume_note.md"),
            [
                (
                    branding.rust_version.as_str(),
                    "Rust-branded version string",
                ),
                (branding.client_bin.as_str(), "client binary name"),
                (branding.daemon_bin.as_str(), "daemon binary name"),
            ],
        ),
        DocumentCheck::new(
            workspace.join("AGENTS.md"),
            [
                (
                    branding.rust_version.as_str(),
                    "Rust-branded version string",
                ),
                (branding.client_bin.as_str(), "client binary name"),
                (branding.daemon_bin.as_str(), "daemon binary name"),
                (daemon_config.as_str(), "daemon configuration path"),
                (branding.source.as_str(), "source repository URL"),
            ],
        ),
    ];

    let mut failures = Vec::new();
    for check in &checks {
        let contents = read_file(&check.path)?;
        for requirement in &check.requirements {
            ensure_contains(
                workspace,
                &mut failures,
                &check.path,
                &contents,
                requirement.fragment,
                requirement.description,
            );
        }
    }

    validate_cross_compile_sections(workspace, &branding, &mut failures)?;
    validate_ci_cross_compile_matrix(workspace, &branding, &mut failures)?;

    if failures.is_empty() {
        Ok(())
    } else {
        Err(TaskError::Validation(format!(
            "documentation validation failed:\n{}",
            failures.join("\n")
        )))
    }
}

struct DocumentCheck<'a> {
    path: PathBuf,
    requirements: Vec<DocumentRequirement<'a>>,
}

impl<'a> DocumentCheck<'a> {
    fn new(path: PathBuf, requirements: impl IntoIterator<Item = (&'a str, &'static str)>) -> Self {
        Self {
            path,
            requirements: requirements
                .into_iter()
                .map(|(fragment, description)| DocumentRequirement {
                    fragment,
                    description,
                })
                .collect(),
        }
    }
}

struct DocumentRequirement<'a> {
    fragment: &'a str,
    description: &'static str,
}

fn read_file(path: &Path) -> TaskResult<String> {
    fs::read_to_string(path).map_err(|error| {
        let message = format!("failed to read {}: {}", path.display(), error);
        TaskError::Io(io::Error::new(error.kind(), message))
    })
}

fn ensure_contains(
    workspace: &Path,
    failures: &mut Vec<String>,
    path: &Path,
    contents: &str,
    needle: &str,
    description: &str,
) {
    if !contents.contains(needle) {
        let display_path = path
            .strip_prefix(workspace)
            .map(|relative| relative.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        failures.push(format!(
            "{display_path}: missing {description} ('{needle}')"
        ));
    }
}

fn validate_cross_compile_sections(
    workspace: &Path,
    branding: &WorkspaceBranding,
    failures: &mut Vec<String>,
) -> TaskResult<()> {
    let docs = [
        workspace.join("docs").join("production_scope_p1.md"),
        workspace.join("docs").join("feature_matrix.md"),
    ];

    for path in &docs {
        let contents = read_file(path)?;
        for (os, archs) in &branding.cross_compile {
            let formatted = format!("{} ({})", display_os_name(os), archs.join(", "));
            ensure_contains(
                workspace,
                failures,
                path,
                &contents,
                &formatted,
                "cross-compilation target listing",
            );
        }

        if contents.contains("Windows (x86,") || contents.contains("Windows (aarch64") {
            let display_path = path
                .strip_prefix(workspace)
                .map(|relative| relative.display().to_string())
                .unwrap_or_else(|_| path.display().to_string());
            failures.push(format!(
                "{display_path}: references disabled Windows cross-compilation targets"
            ));
        }
    }

    Ok(())
}

fn validate_ci_cross_compile_matrix(
    workspace: &Path,
    branding: &WorkspaceBranding,
    failures: &mut Vec<String>,
) -> TaskResult<()> {
    let ci_path = workspace.join(".github").join("workflows").join("ci.yml");
    let ci_contents = read_file(&ci_path)?;
    let display_path = ci_path
        .strip_prefix(workspace)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| ci_path.display().to_string());

    for (os, arches) in &branding.cross_compile {
        for arch in arches {
            match expected_matrix_name(os, arch) {
                Some(name) => ensure_matrix_entry(
                    failures,
                    &display_path,
                    &ci_contents,
                    &name,
                    MatrixExpectations {
                        enabled: Some(true),
                        target: expected_target(os, arch),
                        build_command: Some(expected_build_command(os)),
                        build_daemon: Some(expected_build_daemon(os, arch)),
                        uses_zig: Some(expected_uses_zig(os)),
                        generate_sbom: Some(expected_generate_sbom(os)),
                        needs_cross_gcc: expected_needs_cross_gcc(os, arch),
                    },
                ),
                None => failures.push(format!(
                    "{display_path}: unrecognised cross-compile platform '{os}' in workspace metadata"
                )),
            }
        }
    }

    ensure_matrix_entry(
        failures,
        &display_path,
        &ci_contents,
        "windows-x86",
        MatrixExpectations {
            enabled: Some(false),
            target: expected_target("windows", "x86"),
            build_command: Some("zigbuild"),
            build_daemon: Some(false),
            uses_zig: Some(true),
            generate_sbom: Some(false),
            needs_cross_gcc: Some(false),
        },
    );
    ensure_matrix_entry(
        failures,
        &display_path,
        &ci_contents,
        "windows-aarch64",
        MatrixExpectations {
            enabled: Some(false),
            target: expected_target("windows", "aarch64"),
            build_command: Some("zigbuild"),
            build_daemon: Some(false),
            uses_zig: Some(true),
            generate_sbom: Some(false),
            needs_cross_gcc: Some(false),
        },
    );

    Ok(())
}

fn ensure_matrix_entry(
    failures: &mut Vec<String>,
    display_path: &str,
    contents: &str,
    name: &str,
    expectations: MatrixExpectations<'_>,
) {
    match extract_matrix_entry(contents, name) {
        Some(entry) => {
            if let Some(expected) = expectations.enabled {
                if entry.enabled != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected enabled={expected}"
                    ));
                }
            }

            if let Some(expected) = expectations.target {
                if entry.target.as_deref() != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected target='{expected}'"
                    ));
                }
            }

            if let Some(expected) = expectations.build_command {
                if entry.build_command.as_deref() != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected build_command='{expected}'"
                    ));
                }
            }

            if let Some(expected) = expectations.build_daemon {
                if entry.build_daemon != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected build_daemon={expected}"
                    ));
                }
            }

            if let Some(expected) = expectations.uses_zig {
                if entry.uses_zig != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected uses_zig={expected}"
                    ));
                }
            }

            if let Some(expected) = expectations.generate_sbom {
                if entry.generate_sbom != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected generate_sbom={expected}"
                    ));
                }
            }

            if let Some(expected) = expectations.needs_cross_gcc {
                if entry.needs_cross_gcc != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected needs_cross_gcc={expected}"
                    ));
                }
            }
        }
        None => failures.push(format!(
            "{display_path}: missing cross-compilation entry '{name}'"
        )),
    }
}

fn expected_matrix_name(os: &str, arch: &str) -> Option<String> {
    match os {
        "linux" => Some(format!("linux-{arch}")),
        "macos" => Some(format!("darwin-{arch}")),
        "windows" => Some(format!("windows-{arch}")),
        _ => None,
    }
}

fn expected_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-gnu"),
        ("windows", "x86") => Some("i686-pc-windows-gnu"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

fn expected_build_command(os: &str) -> &'static str {
    match os {
        "linux" => "build",
        "macos" | "windows" => "zigbuild",
        _ => "build",
    }
}

fn expected_build_daemon(os: &str, _arch: &str) -> bool {
    !matches!(os, "windows")
}

fn expected_uses_zig(os: &str) -> bool {
    matches!(os, "macos" | "windows")
}

fn expected_generate_sbom(os: &str) -> bool {
    !matches!(os, "windows")
}

fn expected_needs_cross_gcc(os: &str, arch: &str) -> Option<bool> {
    match (os, arch) {
        ("linux", "aarch64") => Some(true),
        ("linux", _) => Some(false),
        _ => Some(false),
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct MatrixEntry {
    enabled: Option<bool>,
    target: Option<String>,
    build_command: Option<String>,
    build_daemon: Option<bool>,
    uses_zig: Option<bool>,
    generate_sbom: Option<bool>,
    needs_cross_gcc: Option<bool>,
}

#[derive(Clone, Debug, Default)]
struct MatrixExpectations<'a> {
    enabled: Option<bool>,
    target: Option<&'a str>,
    build_command: Option<&'a str>,
    build_daemon: Option<bool>,
    uses_zig: Option<bool>,
    generate_sbom: Option<bool>,
    needs_cross_gcc: Option<bool>,
}

fn extract_matrix_entry(contents: &str, name: &str) -> Option<MatrixEntry> {
    let mut entry = None;
    let mut capturing = false;

    for line in contents.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- name:") {
            if capturing {
                break;
            }

            if rest.trim() == name {
                entry = Some(MatrixEntry::default());
                capturing = true;
            }
            continue;
        }

        if !capturing {
            continue;
        }

        if let Some(current) = entry.as_mut() {
            let trimmed = trimmed.trim();
            if let Some(enabled) = trimmed.strip_prefix("enabled:") {
                current.enabled = Some(matches!(enabled.trim(), "true"));
            } else if let Some(target) = trimmed.strip_prefix("target:") {
                current.target = Some(target.trim().to_owned());
            } else if let Some(build_command) = trimmed.strip_prefix("build_command:") {
                current.build_command = Some(build_command.trim().to_owned());
            } else if let Some(build_daemon) = trimmed.strip_prefix("build_daemon:") {
                current.build_daemon = parse_bool(build_daemon);
            } else if let Some(uses_zig) = trimmed.strip_prefix("uses_zig:") {
                current.uses_zig = parse_bool(uses_zig);
            } else if let Some(generate_sbom) = trimmed.strip_prefix("generate_sbom:") {
                current.generate_sbom = parse_bool(generate_sbom);
            } else if let Some(needs_cross_gcc) = trimmed.strip_prefix("needs_cross_gcc:") {
                current.needs_cross_gcc = parse_bool(needs_cross_gcc);
            }
        }
    }

    entry
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn display_os_name(os: &str) -> &str {
    match os {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, DocsOptions::default());
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_duplicate_open_flags() {
        let error = parse_args([OsString::from("--open"), OsString::from("--open")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--open")));
    }

    #[test]
    fn parse_args_accepts_validate_flag() {
        let options =
            parse_args([OsString::from("--validate")]).expect("parse succeeds with validate");
        assert!(options.validate);
        assert!(!options.open);
    }

    #[test]
    fn parse_args_rejects_duplicate_validate_flags() {
        let error =
            parse_args([OsString::from("--validate"), OsString::from("--validate")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--validate")));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }

    #[test]
    fn extract_matrix_entry_parses_enabled_flag() {
        let contents = r#"
          - name: linux-x86_64
            enabled: true
            target: x86_64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: false
            generate_sbom: true
          - name: windows-x86
            enabled: false
            target: i686-pc-windows-gnu
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
        "#;

        let linux = extract_matrix_entry(contents, "linux-x86_64").expect("linux entry");
        assert_eq!(linux.enabled, Some(true));
        assert_eq!(linux.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(linux.build_command.as_deref(), Some("build"));
        assert_eq!(linux.build_daemon, Some(true));
        assert_eq!(linux.uses_zig, Some(false));
        assert_eq!(linux.needs_cross_gcc, Some(false));
        assert_eq!(linux.generate_sbom, Some(true));

        let windows = extract_matrix_entry(contents, "windows-x86").expect("windows entry");
        assert_eq!(windows.enabled, Some(false));
        assert_eq!(windows.target.as_deref(), Some("i686-pc-windows-gnu"));
        assert_eq!(windows.build_command.as_deref(), Some("zigbuild"));
        assert_eq!(windows.build_daemon, Some(false));
        assert_eq!(windows.uses_zig, Some(true));
        assert_eq!(windows.needs_cross_gcc, Some(false));
        assert_eq!(windows.generate_sbom, Some(false));
    }

    #[test]
    fn validate_ci_cross_compile_matrix_detects_missing_entries() {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let workspace =
            std::env::temp_dir().join(format!("xtask_docs_ci_validate_{unique_suffix}"));
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup stale workspace");
        }
        fs::create_dir_all(workspace.join(".github").join("workflows")).expect("create workflows");

        fs::write(
            workspace.join("Cargo.toml"),
            r#"[workspace]
members = []
[workspace.metadata]
[workspace.metadata.oc_rsync]
brand = "oc"
upstream_version = "3.4.1"
rust_version = "3.4.1-rust"
protocol = 32
client_bin = "oc-rsync"
daemon_bin = "oc-rsyncd"
legacy_client_bin = "rsync"
legacy_daemon_bin = "rsyncd"
daemon_config_dir = "/etc/oc-rsyncd"
daemon_config = "/etc/oc-rsyncd/oc-rsyncd.conf"
daemon_secrets = "/etc/oc-rsyncd/oc-rsyncd.secrets"
legacy_daemon_config_dir = "/etc"
legacy_daemon_config = "/etc/rsyncd.conf"
legacy_daemon_secrets = "/etc/rsyncd.secrets"
source = "https://github.com/oferchen/rsync"
[workspace.metadata.oc_rsync.cross_compile]
linux = ["x86_64", "aarch64"]
macos = ["x86_64", "aarch64"]
windows = ["x86_64"]
"#,
        )
        .expect("write manifest");

        fs::write(
            workspace.join(".github").join("workflows").join("ci.yml"),
            r#"name: CI

jobs:
  cross-compile:
    strategy:
      matrix:
        platform:
          - name: linux-x86_64
            enabled: true
            target: x86_64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: false
            generate_sbom: true
          - name: linux-aarch64
            enabled: true
            target: aarch64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: true
            generate_sbom: true
          - name: darwin-x86_64
            enabled: true
            target: x86_64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: true
          - name: darwin-aarch64
            enabled: true
            target: aarch64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: true
          - name: windows-x86
            enabled: false
            target: i686-pc-windows-gnu
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
          - name: windows-aarch64
            enabled: false
            target: aarch64-pc-windows-msvc
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
"#,
        )
        .expect("write ci workflow");

        let branding = load_workspace_branding(&workspace).expect("load branding");
        let mut failures = Vec::new();
        validate_ci_cross_compile_matrix(&workspace, &branding, &mut failures)
            .expect("validation completes");
        assert!(
            failures
                .iter()
                .any(|message| message.contains("windows-x86_64")),
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn validate_ci_cross_compile_matrix_detects_mismatched_fields() {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("xtask_docs_ci_fields_{unique_suffix}"));
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup stale workspace");
        }
        fs::create_dir_all(workspace.join(".github").join("workflows")).expect("create workflows");

        fs::write(
            workspace.join("Cargo.toml"),
            r#"[workspace]
members = []
[workspace.metadata]
[workspace.metadata.oc_rsync]
brand = "oc"
upstream_version = "3.4.1"
rust_version = "3.4.1-rust"
protocol = 32
client_bin = "oc-rsync"
daemon_bin = "oc-rsyncd"
legacy_client_bin = "rsync"
legacy_daemon_bin = "rsyncd"
daemon_config_dir = "/etc/oc-rsyncd"
daemon_config = "/etc/oc-rsyncd/oc-rsyncd.conf"
daemon_secrets = "/etc/oc-rsyncd/oc-rsyncd.secrets"
legacy_daemon_config_dir = "/etc"
legacy_daemon_config = "/etc/rsyncd.conf"
legacy_daemon_secrets = "/etc/rsyncd.secrets"
source = "https://github.com/oferchen/rsync"
[workspace.metadata.oc_rsync.cross_compile]
linux = ["x86_64", "aarch64"]
macos = ["x86_64", "aarch64"]
windows = ["x86_64"]
"#,
        )
        .expect("write manifest");

        fs::write(
            workspace.join(".github").join("workflows").join("ci.yml"),
            r#"name: CI

jobs:
  cross-compile:
    strategy:
      matrix:
        platform:
          - name: linux-x86_64
            enabled: true
            target: x86_64-unknown-linux-gnu
            build_command: zigbuild
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: false
            generate_sbom: true
          - name: linux-aarch64
            enabled: true
            target: aarch64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: true
            generate_sbom: true
          - name: darwin-x86_64
            enabled: true
            target: x86_64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: true
          - name: darwin-aarch64
            enabled: true
            target: aarch64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: true
          - name: windows-x86_64
            enabled: true
            target: x86_64-pc-windows-gnu
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
          - name: windows-x86
            enabled: false
            target: i686-pc-windows-gnu
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
          - name: windows-aarch64
            enabled: false
            target: aarch64-pc-windows-msvc
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
"#,
        )
        .expect("write ci workflow");

        let branding = load_workspace_branding(&workspace).expect("branding");
        let mut failures = Vec::new();
        validate_ci_cross_compile_matrix(&workspace, &branding, &mut failures)
            .expect("validation completes");
        assert!(
            failures
                .iter()
                .any(|message| message.contains("linux-x86_64")
                    && message.contains("build_command='build'")),
            "expected build_command validation failure, got {failures:?}"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn validate_ci_cross_compile_matrix_accepts_workspace_configuration() {
        let workspace = crate::workspace::workspace_root().expect("workspace root");
        let branding = load_workspace_branding(&workspace).expect("branding");
        let ci_contents = read_file(&workspace.join(".github").join("workflows").join("ci.yml"))
            .expect("read ci");
        let entry = extract_matrix_entry(&ci_contents, "windows-x86").expect("windows entry");
        assert_eq!(entry.enabled, Some(false));
        let mut failures = Vec::new();
        validate_ci_cross_compile_matrix(&workspace, &branding, &mut failures)
            .expect("validation succeeds");
        assert!(
            failures.is_empty(),
            "unexpected CI validation failures: {failures:?}"
        );
    }

    #[test]
    fn validate_documents_accepts_workspace_branding() {
        let workspace = crate::workspace::workspace_root().expect("workspace root");
        validate_documents(&workspace).expect("documents validate");
    }

    #[test]
    fn validate_documents_reports_missing_branding() {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("xtask_docs_validate_{unique_suffix}"));
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup stale workspace");
        }
        fs::create_dir(&workspace).expect("create workspace");
        fs::create_dir(workspace.join("docs")).expect("create docs directory");

        let manifest = r#"[workspace]
members = []
[workspace.metadata]
[workspace.metadata.oc_rsync]
brand = "oc"
upstream_version = "3.4.1"
rust_version = "3.4.1-rust"
protocol = 32
client_bin = "oc-rsync"
daemon_bin = "oc-rsyncd"
legacy_client_bin = "rsync"
legacy_daemon_bin = "rsyncd"
daemon_config_dir = "/etc/oc-rsyncd"
daemon_config = "/etc/oc-rsyncd/oc-rsyncd.conf"
daemon_secrets = "/etc/oc-rsyncd/oc-rsyncd.secrets"
legacy_daemon_config_dir = "/etc"
legacy_daemon_config = "/etc/rsyncd.conf"
legacy_daemon_secrets = "/etc/rsyncd.secrets"
source = "https://github.com/oferchen/rsync"
[workspace.metadata.oc_rsync.cross_compile]
linux = ["x86_64", "aarch64"]
macos = ["x86_64", "aarch64"]
windows = ["x86_64"]
"#;
        fs::write(workspace.join("Cargo.toml"), manifest).expect("write manifest");
        fs::write(workspace.join("README.md"), "placeholder").expect("write README");
        fs::write(
            workspace.join("docs").join("production_scope_p1.md"),
            "placeholder",
        )
        .expect("write production scope");
        fs::write(
            workspace.join("docs").join("feature_matrix.md"),
            "placeholder",
        )
        .expect("write feature matrix");
        fs::write(workspace.join("docs").join("differences.md"), "placeholder")
            .expect("write differences");
        fs::write(workspace.join("docs").join("gaps.md"), "placeholder").expect("write gaps");
        fs::write(workspace.join("docs").join("resume_note.md"), "placeholder")
            .expect("write resume note");
        fs::write(workspace.join("AGENTS.md"), "placeholder").expect("write agents");
        fs::create_dir_all(workspace.join(".github").join("workflows"))
            .expect("create workflow directory");
        fs::write(
            workspace.join(".github").join("workflows").join("ci.yml"),
            r#"jobs:
  cross-compile:
    strategy:
      matrix:
        platform:
          - name: linux-x86_64
            enabled: true
          - name: linux-aarch64
            enabled: true
          - name: darwin-x86_64
            enabled: true
          - name: darwin-aarch64
            enabled: true
          - name: windows-x86_64
            enabled: true
          - name: windows-x86
            enabled: false
          - name: windows-aarch64
            enabled: false
"#,
        )
        .expect("write ci workflow");

        let error = validate_documents(&workspace).expect_err("validation should fail");
        match error {
            TaskError::Validation(message) => {
                assert!(message.contains("README.md"), "{message}");
                assert!(message.contains("production_scope_p1.md"), "{message}");
                assert!(message.contains("feature_matrix.md"), "{message}");
                assert!(message.contains("differences.md"), "{message}");
                assert!(message.contains("gaps.md"), "{message}");
                assert!(message.contains("resume_note.md"), "{message}");
                assert!(message.contains("AGENTS.md"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }
}
