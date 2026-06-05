//! Template rendering for release notes.
//!
//! Reads `.github/RELEASE_TEMPLATE.md` and substitutes `{{VERSION}}` and
//! `{{PREV_VERSION}}` placeholders with concrete tag values.

use crate::error::{TaskError, TaskResult};
use crate::util::read_file_with_context;
use crate::workspace::load_workspace_branding;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Options for the `render` subcommand.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Override the version string (default: from workspace metadata).
    pub version: Option<String>,
    /// Output file path (default: stdout).
    pub output: Option<std::path::PathBuf>,
}

/// Renders the release template with substituted placeholders.
pub fn execute(workspace: &Path, options: RenderOptions) -> TaskResult<()> {
    let version = match options.version {
        Some(v) => {
            let v = v.strip_prefix('v').unwrap_or(&v);
            format!("v{v}")
        }
        None => {
            let branding = load_workspace_branding(workspace)?;
            format!("v{}", branding.rust_version)
        }
    };

    let prev_version = find_previous_tag(workspace, &version)?;

    let template_path = workspace.join(".github/RELEASE_TEMPLATE.md");
    let template = read_file_with_context(&template_path)?;

    let rendered = template
        .replace("{{VERSION}}", &version)
        .replace("{{PREV_VERSION}}", &prev_version);

    match options.output {
        Some(path) => {
            let resolved = if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            };
            std::fs::write(&resolved, &rendered).map_err(|e| {
                TaskError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to write {}: {e}", resolved.display()),
                ))
            })?;
            println!("Rendered release notes to {}", resolved.display());
        }
        None => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            handle.write_all(rendered.as_bytes())?;
        }
    }

    Ok(())
}

/// Finds the most recent `v*` tag before the given version.
///
/// Falls back to `v0.0.0` if no prior tags exist.
fn find_previous_tag(workspace: &Path, current_version: &str) -> TaskResult<String> {
    let output = Command::new("git")
        .args(["tag", "--list", "v*", "--sort=-version:refname"])
        .current_dir(workspace)
        .output()
        .map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to run git tag: {e}"),
            ))
        })?;

    if !output.status.success() {
        return Ok("v0.0.0".to_owned());
    }

    let tags = String::from_utf8_lossy(&output.stdout);
    for tag in tags.lines() {
        let tag = tag.trim();
        if !tag.is_empty() && tag != current_version {
            return Ok(tag.to_owned());
        }
    }

    Ok("v0.0.0".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn create_test_workspace(dir: &Path) {
        let github_dir = dir.join(".github");
        fs::create_dir_all(&github_dir).unwrap();
        fs::write(
            github_dir.join("RELEASE_TEMPLATE.md"),
            "## oc-rsync {{VERSION}}\n\n**Full changelog:** compare/{{PREV_VERSION}}...{{VERSION}}\n",
        )
        .unwrap();
    }

    #[test]
    fn render_substitutes_placeholders() {
        let dir = tempdir().unwrap();
        create_test_workspace(dir.path());

        let template_path = dir.path().join(".github/RELEASE_TEMPLATE.md");
        let template = read_file_with_context(&template_path).unwrap();

        let rendered = template
            .replace("{{VERSION}}", "v0.7.0")
            .replace("{{PREV_VERSION}}", "v0.6.3");

        assert!(rendered.contains("## oc-rsync v0.7.0"));
        assert!(rendered.contains("compare/v0.6.3...v0.7.0"));
        assert!(!rendered.contains("{{VERSION}}"));
        assert!(!rendered.contains("{{PREV_VERSION}}"));
    }

    #[test]
    fn render_to_file_creates_output() {
        let dir = tempdir().unwrap();
        create_test_workspace(dir.path());

        // Initialize a git repo so find_previous_tag works
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["tag", "v0.5.0"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let output_path = dir.path().join("RELEASE.md");
        let options = RenderOptions {
            version: Some("0.7.0".to_owned()),
            output: Some(output_path.clone()),
        };

        execute(dir.path(), options).unwrap();

        let content = fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("## oc-rsync v0.7.0"));
        assert!(content.contains("v0.5.0"));
    }

    #[test]
    fn render_version_prefix_normalization() {
        let dir = tempdir().unwrap();
        create_test_workspace(dir.path());

        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let output_path = dir.path().join("RELEASE.md");

        // With v prefix
        let options = RenderOptions {
            version: Some("v1.0.0".to_owned()),
            output: Some(output_path.clone()),
        };
        execute(dir.path(), options).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("v1.0.0"));
        assert!(!content.contains("vv1.0.0"));

        // Without v prefix
        let options = RenderOptions {
            version: Some("1.0.0".to_owned()),
            output: Some(output_path.clone()),
        };
        execute(dir.path(), options).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("v1.0.0"));
    }

    #[test]
    fn find_previous_tag_falls_back_to_v0() {
        let dir = tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tag = find_previous_tag(dir.path(), "v1.0.0").unwrap();
        assert_eq!(tag, "v0.0.0");
    }

    #[test]
    fn find_previous_tag_skips_current_version() {
        let dir = tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["tag", "v0.5.0"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["tag", "v0.6.0"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tag = find_previous_tag(dir.path(), "v0.6.0").unwrap();
        assert_eq!(tag, "v0.5.0");
    }
}
