use crate::error::TaskError;
use std::env;
use std::io;

pub(crate) const FORCE_MISSING_ENV: &str = "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS";

pub(crate) fn should_simulate_missing_tool(display: &str) -> bool {
    let Ok(entries) = env::var(FORCE_MISSING_ENV) else {
        return false;
    };

    entries
        .split(|ch| matches!(ch, ',' | ';' | '|'))
        .map(str::trim)
        .any(|value| !value.is_empty() && value == display)
}

pub(crate) fn map_command_error(error: io::Error, program: &str, install_hint: &str) -> TaskError {
    if error.kind() == io::ErrorKind::NotFound {
        TaskError::ToolMissing(format!("{program} is unavailable; {install_hint}"))
    } else {
        TaskError::Io(error)
    }
}

pub(crate) fn tool_missing_error(display: &str, install_hint: &str) -> TaskError {
    TaskError::ToolMissing(format!("{display} is unavailable; {install_hint}"))
}

#[cfg(test)]
mod tests {
    use super::{FORCE_MISSING_ENV, should_simulate_missing_tool};
    #[test]
    fn simulation_checks_match_exact_entries() {
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(FORCE_MISSING_ENV);
        }
        assert!(!should_simulate_missing_tool("cargo fmt"));

        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(FORCE_MISSING_ENV, "cargo fmt,git status");
        }
        assert!(should_simulate_missing_tool("cargo fmt"));
        assert!(should_simulate_missing_tool("git status"));
        assert!(!should_simulate_missing_tool("cargo"));
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(FORCE_MISSING_ENV);
        }
    }
}
