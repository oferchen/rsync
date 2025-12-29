//! Utility helpers shared across `xtask` subcommands.
//!
//! The previous monolithic module exceeded the repository's enforced line
//! count.  This directory-based layout keeps related helpers grouped in small
//! files so future additions remain manageable.

mod cargo;
mod commands;
mod env;
mod filesystem;
mod git;
mod limits;
#[cfg(test)]
pub mod test_env;

pub use cargo::cargo_metadata_json;
pub use commands::{
    ensure_command_available, ensure_rust_target_installed, probe_cargo_tool, run_cargo_tool,
    run_cargo_tool_with_env,
};
pub use filesystem::{count_file_lines, is_probably_binary};
pub use git::{list_rust_sources_via_git, list_tracked_files};
pub use limits::read_limit_env_var;

use crate::error::{TaskError, TaskResult};

/// Ensures the provided condition holds, returning a [`TaskError::Validation`]
/// otherwise.
pub fn ensure(condition: bool, message: impl Into<String>) -> TaskResult<()> {
    if condition {
        Ok(())
    } else {
        Err(validation_error(message))
    }
}

/// Constructs a [`TaskError::Validation`] using the provided message.
#[must_use]
pub fn validation_error(message: impl Into<String>) -> TaskError {
    TaskError::Validation(message.into())
}

#[cfg(test)]
mod tests {
    use super::{ensure, validation_error};
    use crate::error::TaskError;

    #[test]
    fn ensure_reports_validation_failure() {
        ensure(true, "unused message").expect("true condition succeeds");
        let error = ensure(false, "failure").unwrap_err();
        assert!(matches!(error, TaskError::Validation(message) if message == "failure"));
    }

    #[test]
    fn validation_error_constructs_validation_variant() {
        let error = validation_error("invalid");
        assert!(matches!(error, TaskError::Validation(message) if message == "invalid"));
    }
}
