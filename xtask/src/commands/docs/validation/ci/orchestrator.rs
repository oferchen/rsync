use super::super::read_file;
use super::yaml::{extract_job_section, find_yaml_scalar};
use crate::error::TaskResult;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(super) fn validate_ci_orchestrator(
    workspace: &Path,
    failures: &mut Vec<String>,
) -> TaskResult<()> {
    let path = workflow_path(workspace);
    let contents = read_file(&path)?;
    let display_path = display_path(workspace, &path);

    let mut observed_jobs = collect_job_names(&contents);
    let mut expected_jobs = BTreeSet::new();

    for spec in EXPECTED_JOBS {
        expected_jobs.insert(spec.name);
        validate_job_section(&display_path, &contents, spec, failures);
    }

    observed_jobs.retain(|job| !job.starts_with('#'));

    let mut seen_jobs = BTreeSet::new();

    for job in &observed_jobs {
        if !seen_jobs.insert(job.clone()) {
            failures.push(format!("{display_path}: duplicate CI job '{job}'"));
            continue;
        }

        if !expected_jobs.contains(job.as_str()) {
            failures.push(format!(
                "{display_path}: unexpected CI job '{job}' (only lint/test-linux/cross-compile/interop are allowed)"
            ));
        }
    }

    for expected in expected_jobs {
        if !observed_jobs.iter().any(|job| job.as_str() == expected) {
            failures.push(format!(
                "{display_path}: missing required CI job '{expected}'"
            ));
        }
    }

    Ok(())
}

struct JobSpec {
    name: &'static str,
    workflow: &'static str,
    needs: Option<&'static str>,
}

const EXPECTED_JOBS: &[JobSpec] = &[
    JobSpec {
        name: "lint",
        workflow: "./.github/workflows/lint.yml",
        needs: None,
    },
    JobSpec {
        name: "test-linux",
        workflow: "./.github/workflows/test-linux.yml",
        needs: Some("lint"),
    },
    JobSpec {
        name: "cross-compile",
        workflow: "./.github/workflows/cross-compile.yml",
        needs: Some("lint"),
    },
    JobSpec {
        name: "interop",
        workflow: "./.github/workflows/interop.yml",
        needs: Some("lint"),
    },
];

fn validate_job_section(
    display_path: &str,
    contents: &str,
    spec: &JobSpec,
    failures: &mut Vec<String>,
) {
    let Some(section) = extract_job_section(contents, spec.name) else {
        failures.push(format!(
            "{display_path}: missing CI job definition '{name}'",
            name = spec.name
        ));
        return;
    };

    match find_yaml_scalar(&section, "uses") {
        Some(value) if value == spec.workflow => {}
        Some(value) => failures.push(format!(
            "{display_path}: job '{name}' must use workflow '{expected}' (found '{value}')",
            name = spec.name,
            expected = spec.workflow,
        )),
        None => failures.push(format!(
            "{display_path}: job '{name}' missing 'uses' reference to reusable workflow",
            name = spec.name
        )),
    }

    match find_yaml_scalar(&section, "secrets") {
        Some(value) if value == "inherit" => {}
        Some(value) => failures.push(format!(
            "{display_path}: job '{name}' must inherit secrets (found '{value}')",
            name = spec.name
        )),
        None => failures.push(format!(
            "{display_path}: job '{name}' missing secrets inheritance",
            name = spec.name
        )),
    }

    match (spec.needs, find_yaml_scalar(&section, "needs")) {
        (Some(expected), Some(value)) if value == expected => {}
        (Some(expected), Some(value)) => failures.push(format!(
            "{display_path}: job '{name}' must depend on '{expected}' (found '{value}')",
            name = spec.name
        )),
        (Some(expected), None) => failures.push(format!(
            "{display_path}: job '{name}' missing required dependency '{expected}'",
            name = spec.name
        )),
        (None, Some(value)) if value.is_empty() => {}
        (None, Some(value)) => failures.push(format!(
            "{display_path}: job '{name}' must not declare dependencies (found '{value}')",
            name = spec.name
        )),
        (None, None) => {}
    }
}

fn collect_job_names(contents: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_jobs_section = false;

    for line in contents.lines() {
        let indent = line.chars().take_while(|c| *c == ' ').count();
        let trimmed = line[indent..].trim_end();
        if indent == 0 && trimmed == "jobs:" {
            in_jobs_section = true;
            continue;
        }

        if indent == 0 && trimmed.ends_with(':') && trimmed != "jobs:" {
            in_jobs_section = false;
        }

        if !in_jobs_section || indent != 2 {
            continue;
        }

        if trimmed.starts_with('#') || !trimmed.ends_with(':') {
            continue;
        }

        let name = trimmed.trim_end_matches(':');
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }

    names
}

fn workflow_path(workspace: &Path) -> PathBuf {
    workspace.join(".github").join("workflows").join("ci.yml")
}

fn display_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
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
windows = ["x86_64", "aarch64"]
"#;

    fn unique_workspace(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{suffix}"))
    }

    fn write_manifest(workspace: &Path) {
        if !workspace.exists() {
            fs::create_dir_all(workspace).expect("create workspace root");
        }
        fs::write(workspace.join("Cargo.toml"), MANIFEST_SNIPPET).expect("write manifest");
    }

    fn write_ci_file(workspace: &Path, contents: &str) {
        let workflows = workspace.join(".github").join("workflows");
        fs::create_dir_all(&workflows).expect("create workflows");
        fs::write(workflows.join("ci.yml"), contents).expect("write ci workflow");
    }

    #[test]
    fn validate_ci_orchestrator_requires_expected_jobs() {
        let workspace = unique_workspace("xtask_docs_ci_orchestrator_missing");
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup workspace");
        }

        write_manifest(&workspace);
        write_ci_file(
            &workspace,
            r#"jobs:
  lint:
    uses: ./.github/workflows/lint.yml
    secrets: inherit
"#,
        );

        let mut failures = Vec::new();
        validate_ci_orchestrator(&workspace, &mut failures).expect("validation completes");
        assert!(
            failures
                .iter()
                .any(|message| message.contains("missing required CI job 'test-linux'")),
            "expected missing-job failure, got {failures:?}"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn validate_ci_orchestrator_enforces_dependencies() {
        let workspace = unique_workspace("xtask_docs_ci_orchestrator_needs");
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup workspace");
        }

        write_manifest(&workspace);
        write_ci_file(
            &workspace,
            r#"jobs:
  lint:
    uses: ./.github/workflows/lint.yml
    secrets: inherit
  test-linux:
    uses: ./.github/workflows/test-linux.yml
    secrets: inherit
"#,
        );

        let mut failures = Vec::new();
        validate_ci_orchestrator(&workspace, &mut failures).expect("validation completes");
        assert!(
            failures
                .iter()
                .any(|message| message.contains("missing required dependency 'lint'")),
            "expected dependency failure, got {failures:?}"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn validate_ci_orchestrator_enforces_secret_inheritance() {
        let workspace = unique_workspace("xtask_docs_ci_orchestrator_secrets");
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup workspace");
        }

        write_manifest(&workspace);
        write_ci_file(
            &workspace,
            r#"jobs:
  lint:
    uses: ./.github/workflows/lint.yml
    secrets: inherit
  test-linux:
    needs: lint
    uses: ./.github/workflows/test-linux.yml
  cross-compile:
    needs: lint
    uses: ./.github/workflows/cross-compile.yml
    secrets: inherit
  interop:
    needs: lint
    uses: ./.github/workflows/interop.yml
    secrets: inherit
"#,
        );

        let mut failures = Vec::new();
        validate_ci_orchestrator(&workspace, &mut failures).expect("validation completes");
        assert!(
            failures
                .iter()
                .any(|message| message.contains("missing secrets inheritance")),
            "expected secret-inheritance failure, got {failures:?}"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn validate_ci_orchestrator_rejects_extra_jobs() {
        let workspace = unique_workspace("xtask_docs_ci_orchestrator_extra");
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup workspace");
        }

        write_manifest(&workspace);
        write_ci_file(
            &workspace,
            r#"jobs:
  lint:
    uses: ./.github/workflows/lint.yml
    secrets: inherit
  test-linux:
    needs: lint
    uses: ./.github/workflows/test-linux.yml
    secrets: inherit
  cross-compile:
    needs: lint
    uses: ./.github/workflows/cross-compile.yml
    secrets: inherit
  interop:
    needs: lint
    uses: ./.github/workflows/interop.yml
    secrets: inherit
  docs:
    uses: ./.github/workflows/docs.yml
    secrets: inherit
"#,
        );

        let mut failures = Vec::new();
        validate_ci_orchestrator(&workspace, &mut failures).expect("validation completes");
        assert!(
            failures
                .iter()
                .any(|message| message.contains("unexpected CI job 'docs'")),
            "expected unexpected-job failure, got {failures:?}"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn validate_ci_orchestrator_accepts_workspace_configuration() {
        let workspace = crate::workspace::workspace_root().expect("workspace root");
        let mut failures = Vec::new();
        validate_ci_orchestrator(&workspace, &mut failures).expect("validation succeeds");
        assert!(
            failures.is_empty(),
            "unexpected CI orchestrator failures: {failures:?}"
        );
    }
}
