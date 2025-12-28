use super::super::read_file;
use super::yaml::{extract_job_section, find_yaml_scalar};
use crate::error::TaskResult;
use std::path::Path;

pub(super) fn validate_ci_test_job(workspace: &Path, failures: &mut Vec<String>) -> TaskResult<()> {
    let ci_path = workspace
        .join(".github")
        .join("workflows")
        .join("test-linux.yml");
    let ci_contents = read_file(&ci_path)?;
    let display_path = ci_path.strip_prefix(workspace).map_or_else(
        |_| ci_path.display().to_string(),
        |relative| relative.display().to_string(),
    );

    match extract_job_section(&ci_contents, "test-linux") {
        Some(section) => match find_yaml_scalar(&section, "runs-on") {
            Some(value) if value == "ubuntu-latest" => {}
            Some(value) => failures.push(format!(
                "{display_path}: test-linux job must run on ubuntu-latest (found '{value}')"
            )),
            None => failures.push(format!(
                "{display_path}: test-linux job missing runs-on configuration"
            )),
        },
        None => failures.push(format!("{display_path}: missing test-linux job definition")),
    }

    for line in ci_contents.lines() {
        let indent = line.chars().take_while(|c| *c == ' ').count();
        if indent != 2 {
            continue;
        }

        let trimmed = line[indent..].trim_end();
        if trimmed.starts_with('#') || !trimmed.ends_with(':') {
            continue;
        }

        let name = trimmed.trim_end_matches(':');
        if name.starts_with("test-") && name != "test-linux" {
            failures.push(format!(
                "{display_path}: unexpected test job '{name}' (tests must run on Linux only)"
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_workspace(prefix: &str) -> std::path::PathBuf {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{unique_suffix}"))
    }

    fn write_manifest(workspace: &Path) {
        if !workspace.exists() {
            std::fs::create_dir_all(workspace).expect("create workspace root");
        }

        let manifest = test_support::manifest_snippet();
        std::fs::write(workspace.join("Cargo.toml"), manifest).expect("write manifest");
    }

    fn write_ci_file(workspace: &Path, contents: &str) {
        let workflows = workspace.join(".github").join("workflows");
        std::fs::create_dir_all(&workflows).expect("create workflows");
        std::fs::write(workflows.join("test-linux.yml"), contents).expect("write ci workflow");
    }

    #[test]
    fn validate_ci_test_job_requires_linux_runner() {
        let workspace = unique_workspace("xtask_docs_ci_linux_runner");
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup workspace");
        }

        write_manifest(&workspace);
        write_ci_file(
            &workspace,
            r#"jobs:
  test-linux:
    runs-on: macos-latest
"#,
        );

        let mut failures = Vec::new();
        validate_ci_test_job(&workspace, &mut failures).expect("validation completes");
        assert!(
            failures
                .iter()
                .any(|message| message.contains("test-linux job must run on ubuntu-latest")),
            "expected linux-runner validation failure, got {failures:?}"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn validate_ci_test_job_rejects_additional_test_jobs() {
        let workspace = unique_workspace("xtask_docs_ci_extra_tests");
        if workspace.exists() {
            fs::remove_dir_all(&workspace).expect("cleanup workspace");
        }

        write_manifest(&workspace);
        write_ci_file(
            &workspace,
            r#"jobs:
  test-linux:
    runs-on: ubuntu-latest
  test-macos:
    runs-on: macos-latest
"#,
        );

        let mut failures = Vec::new();
        validate_ci_test_job(&workspace, &mut failures).expect("validation completes");
        assert!(
            failures
                .iter()
                .any(|message| message.contains("unexpected test job 'test-macos'")),
            "expected unexpected-test-job validation failure, got {failures:?}"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    // #[test]
    // fn validate_ci_test_job_accepts_workspace_configuration() {
    //     let workspace = crate::workspace::workspace_root().expect("workspace root");
    //     let mut failures = Vec::new();
    //     validate_ci_test_job(&workspace, &mut failures).expect("validation succeeds");
    //     assert!(
    //         failures.is_empty(),
    //         "unexpected CI test-job failures: {failures:?}"
    //     );
    // }
}
