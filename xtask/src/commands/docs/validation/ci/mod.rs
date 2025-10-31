mod matrix;
mod test_job;
pub(crate) mod yaml;

use crate::error::TaskResult;
use crate::workspace::WorkspaceBranding;
use std::path::Path;

pub(super) fn validate_ci_cross_compile_matrix(
    workspace: &Path,
    branding: &WorkspaceBranding,
    failures: &mut Vec<String>,
) -> TaskResult<()> {
    matrix::validate_ci_cross_compile_matrix(workspace, branding, failures)
}

pub(super) fn validate_ci_test_job(workspace: &Path, failures: &mut Vec<String>) -> TaskResult<()> {
    test_job::validate_ci_test_job(workspace, failures)
}
