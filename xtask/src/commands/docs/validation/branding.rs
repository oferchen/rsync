use super::{ensure_contains, read_file};
use crate::error::TaskResult;
use crate::workspace::WorkspaceBranding;
use std::path::{Path, PathBuf};

pub(super) fn validate_branding_documents(
    workspace: &Path,
    branding: &WorkspaceBranding,
    failures: &mut Vec<String>,
) -> TaskResult<()> {
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
            docs_dir.join("COMPARE.md"),
            [
                (
                    branding.rust_version.as_str(),
                    "Rust-branded version string",
                ),
                (branding.client_bin.as_str(), "client binary name"),
                (branding.daemon_bin.as_str(), "daemon binary name"),
                (daemon_config.as_str(), "daemon configuration path"),
            ],
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

    for check in &checks {
        let contents = read_file(&check.path)?;
        for requirement in &check.requirements {
            ensure_contains(
                workspace,
                failures,
                &check.path,
                &contents,
                requirement.fragment,
                requirement.description,
            );
        }
    }

    validate_cross_compile_sections(workspace, branding, failures)?;

    Ok(())
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
    use crate::workspace::load_workspace_branding;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    const MANIFEST_SNIPPET: &str = r#"[workspace]
members = []
[workspace.metadata]
[workspace.metadata.oc_rsync]
brand = "oc"
upstream_version = "3.4.1"
rust_version = "3.4.1-rust"
protocol = 32
client_bin = "oc-rsync"
daemon_bin = "oc-rsync"
daemon_wrapper_bin = "oc-rsyncd"
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
windows = ["x86_64", "aarch64"]
"#;

    #[test]
    fn validate_branding_documents_reports_missing_content() {
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

        fs::write(workspace.join("Cargo.toml"), MANIFEST_SNIPPET).expect("write manifest");
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
        fs::write(workspace.join("docs").join("COMPARE.md"), "placeholder")
            .expect("write compare log");
        fs::write(workspace.join("docs").join("gaps.md"), "placeholder").expect("write gaps");
        fs::write(workspace.join("docs").join("resume_note.md"), "placeholder")
            .expect("write resume note");
        fs::write(workspace.join("AGENTS.md"), "placeholder").expect("write agents");

        let branding = load_workspace_branding(&workspace).expect("load branding");
        let mut failures = Vec::new();
        validate_branding_documents(&workspace, &branding, &mut failures)
            .expect("validation should complete");

        assert!(failures.iter().any(|message| message.contains("README.md")));
        assert!(
            failures
                .iter()
                .any(|message| message.contains("production_scope_p1.md"))
        );
        assert!(
            failures
                .iter()
                .any(|message| message.contains("feature_matrix.md"))
        );
        assert!(
            failures
                .iter()
                .any(|message| message.contains("differences.md"))
        );
        assert!(failures.iter().any(|message| message.contains("gaps.md")));
        assert!(
            failures
                .iter()
                .any(|message| message.contains("COMPARE.md"))
        );
        assert!(
            failures
                .iter()
                .any(|message| message.contains("resume_note.md"))
        );
        assert!(failures.iter().any(|message| message.contains("AGENTS.md")));

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }
}
