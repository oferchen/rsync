//! Test scenario definitions for exit code validation.

use crate::error::{TaskError, TaskResult};
use serde::Deserialize;
use std::path::Path;

/// A test scenario that validates a specific exit code.
#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    /// Unique name for this scenario.
    pub name: String,
    /// Expected exit code.
    pub exit_code: i32,
    /// Command-line arguments to pass to rsync.
    pub args: Vec<String>,
    /// Optional shell commands to run before executing the test.
    #[serde(default)]
    pub setup: Option<String>,
    /// Description of what this scenario tests.
    #[allow(dead_code)]
    pub description: String,
    /// Whether to skip this scenario (for tests that are hard to trigger).
    #[serde(default)]
    pub skip: bool,
}

/// Container for loading scenarios from TOML.
#[derive(Debug, Deserialize)]
struct ScenariosFile {
    scenario: Vec<Scenario>,
}

/// Load exit code scenarios from the scenarios.toml file.
pub fn load_scenarios(workspace: &Path) -> TaskResult<Vec<Scenario>> {
    let scenarios_path = workspace.join("tests/interop/exit_codes/scenarios.toml");

    if !scenarios_path.exists() {
        return Err(TaskError::Metadata(format!(
            "Scenarios file not found: {}",
            scenarios_path.display()
        )));
    }

    let content = std::fs::read_to_string(&scenarios_path).map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to read {}: {}", scenarios_path.display(), e),
        ))
    })?;

    let scenarios_file: ScenariosFile = toml::from_str(&content)
        .map_err(|e| TaskError::Metadata(format!("Failed to parse scenarios.toml: {}", e)))?;

    Ok(scenarios_file.scenario)
}

/// Filter scenarios to only include those that should be run.
pub fn filter_runnable(scenarios: Vec<Scenario>) -> Vec<Scenario> {
    scenarios.into_iter().filter(|s| !s.skip).collect()
}

/// Find a specific scenario by name.
#[allow(dead_code)]
pub fn find_scenario<'a>(scenarios: &'a [Scenario], name: &str) -> Option<&'a Scenario> {
    scenarios.iter().find(|s| s.name == name)
}
