mod branding;
mod ci;

use crate::error::{TaskError, TaskResult};
use crate::workspace::load_workspace_branding;
use std::path::Path;

/// Re-export `read_file_with_context` under the legacy name used by submodules.
pub(super) use crate::util::read_file_with_context as read_file;

pub(super) fn validate_documents(workspace: &Path) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    let mut failures = Vec::new();

    branding::validate_branding_documents(workspace, &branding, &mut failures)?;
    ci::validate_ci_cross_compile_matrix(workspace, &branding, &mut failures)?;
    ci::validate_ci_orchestrator(workspace, &mut failures)?;
    ci::validate_ci_test_job(workspace, &mut failures)?;

    if failures.is_empty() {
        Ok(())
    } else {
        Err(TaskError::Validation(format!(
            "documentation validation failed:\n{}",
            failures.join("\n")
        )))
    }
}

pub(super) fn ensure_contains(
    workspace: &Path,
    failures: &mut Vec<String>,
    path: &Path,
    contents: &str,
    needle: &str,
    description: &str,
) {
    if !contents.contains(needle) {
        let display_path = path.strip_prefix(workspace).map_or_else(
            |_| path.display().to_string(),
            |relative| relative.display().to_string(),
        );
        failures.push(format!(
            "{display_path}: missing {description} ('{needle}')"
        ));
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::workspace;

//     #[test]
//     fn validate_documents_accepts_workspace_branding() {
//         let workspace = workspace::workspace_root().expect("workspace root");
//         validate_documents(&workspace).expect("documents validate");
//     }
// }
