#![deny(unsafe_code)]

//! Man page generation command.
//!
//! The `man-page` command converts the pandoc-compatible markdown man page
//! source (`docs/oc-rsync.1.md`) into a troff man page (`docs/oc-rsync.1`)
//! using pandoc. If pandoc is not available, the command prints a message
//! and exits successfully to avoid blocking CI on optional documentation
//! tooling.

use crate::error::{TaskError, TaskResult};
use std::path::Path;
use std::process::Command;

/// Source markdown file relative to workspace root.
const MAN_PAGE_SOURCE: &str = "docs/oc-rsync.1.md";

/// Output troff man page relative to workspace root.
const MAN_PAGE_OUTPUT: &str = "docs/oc-rsync.1";

/// Executes the `man-page` command.
///
/// Generates a troff man page from the markdown source using pandoc.
/// If pandoc is not installed, prints a skip message and returns success.
pub fn execute(workspace: &Path) -> TaskResult<()> {
    let source = workspace.join(MAN_PAGE_SOURCE);
    let output = workspace.join(MAN_PAGE_OUTPUT);

    if !source.exists() {
        return Err(TaskError::Validation(format!(
            "man page source not found: {}",
            source.display()
        )));
    }

    if !pandoc_available() {
        println!("pandoc not found; skipping man page generation");
        println!("Install pandoc to generate the man page: https://pandoc.org/installing.html");
        return Ok(());
    }

    println!("Generating man page: {MAN_PAGE_SOURCE} -> {MAN_PAGE_OUTPUT}");

    let status = Command::new("pandoc")
        .arg("-s")
        .arg("-t")
        .arg("man")
        .arg("-o")
        .arg(&output)
        .arg(&source)
        .current_dir(workspace)
        .status()
        .map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to run pandoc: {e}"),
            ))
        })?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: "pandoc".to_owned(),
            status,
        });
    }

    println!("Man page generated: {MAN_PAGE_OUTPUT}");
    Ok(())
}

/// Checks whether pandoc is available on the system PATH.
fn pandoc_available() -> bool {
    Command::new("pandoc")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn man_page_source_path_is_relative() {
        assert!(!MAN_PAGE_SOURCE.starts_with('/'));
    }

    #[test]
    fn man_page_output_path_is_relative() {
        assert!(!MAN_PAGE_OUTPUT.starts_with('/'));
    }

    #[test]
    fn man_page_source_exists_in_workspace() {
        let workspace = crate::workspace::workspace_root().expect("resolve workspace root");
        let source = workspace.join(MAN_PAGE_SOURCE);
        assert!(
            source.exists(),
            "man page source should exist at {MAN_PAGE_SOURCE}"
        );
    }

    #[test]
    fn execute_fails_when_source_missing() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let error = execute(temp.path()).unwrap_err();
        assert!(matches!(error, TaskError::Validation(msg) if msg.contains("not found")));
    }
}
