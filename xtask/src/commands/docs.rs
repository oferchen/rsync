use crate::error::{TaskError, TaskResult};
use crate::util::{is_help_flag, run_cargo_tool};
use crate::workspace::load_workspace_branding;
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

        let error = validate_documents(&workspace).expect_err("validation should fail");
        match error {
            TaskError::Validation(message) => {
                assert!(message.contains("README.md"), "{message}");
                assert!(message.contains("production_scope_p1.md"), "{message}");
                assert!(message.contains("feature_matrix.md"), "{message}");
                assert!(message.contains("differences.md"), "{message}");
                assert!(message.contains("gaps.md"), "{message}");
                assert!(message.contains("resume_note.md"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }
}
