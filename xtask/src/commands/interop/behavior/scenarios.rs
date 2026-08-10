//! Behavior scenario definitions for upstream parity testing.

use crate::error::{TaskError, TaskResult};
use serde::Deserialize;
use std::path::Path;

/// A behavior test scenario that compares oc-rsync vs upstream rsync.
#[derive(Debug, Clone, Deserialize)]
pub struct BehaviorScenario {
    /// Unique name for this scenario.
    pub name: String,

    /// Description of what this scenario tests.
    pub description: String,

    /// Command-line arguments to pass to both rsync implementations.
    /// The first element is typically "rsync" and will be replaced with the actual binary.
    pub args: Vec<String>,

    /// Optional shell commands to run before executing the test.
    /// These run in the test's working directory.
    #[serde(default)]
    pub setup: Option<String>,

    /// What aspects to compare between implementations.
    /// Valid values: "exit_code", "files", "output", "permissions", "symlinks", "hardlinks"
    #[serde(default)]
    pub compare: Vec<String>,

    /// Whether to skip this scenario.
    #[serde(default)]
    pub skip: bool,

    /// Document known behavioral differences.
    /// This field is used for documentation and reporting.
    #[serde(default)]
    #[allow(dead_code)]
    pub known_difference: Option<String>,

    /// Optional cleanup commands to run after the test.
    #[serde(default)]
    pub cleanup: Option<String>,
}

impl BehaviorScenario {
    /// Check if exit codes should be compared.
    pub fn compare_exit_code(&self) -> bool {
        self.compare.is_empty() || self.compare.iter().any(|c| c == "exit_code")
    }

    /// Check if file states should be compared.
    pub fn compare_files(&self) -> bool {
        self.compare.is_empty() || self.compare.iter().any(|c| c == "files")
    }

    /// Check if output should be compared.
    #[allow(dead_code)]
    pub fn compare_output(&self) -> bool {
        self.compare.iter().any(|c| c == "output")
    }

    /// Check if permissions should be compared.
    pub fn compare_permissions(&self) -> bool {
        self.compare.iter().any(|c| c == "permissions")
    }

    /// Check if symlinks should be compared.
    pub fn compare_symlinks(&self) -> bool {
        self.compare.iter().any(|c| c == "symlinks")
    }

    /// Check if hardlinks should be compared.
    pub fn compare_hardlinks(&self) -> bool {
        self.compare.iter().any(|c| c == "hardlinks")
    }
}

/// Container for loading scenarios from TOML.
#[derive(Debug, Deserialize)]
struct ScenariosFile {
    scenario: Vec<BehaviorScenario>,
}

/// Load behavior scenarios from the scenarios.toml file.
pub fn load_scenarios(workspace: &Path) -> TaskResult<Vec<BehaviorScenario>> {
    let scenarios_path = workspace.join("tests/interop/behavior/scenarios.toml");

    if !scenarios_path.exists() {
        return Err(TaskError::Metadata(format!(
            "Behavior scenarios file not found: {}",
            scenarios_path.display()
        )));
    }

    let content = std::fs::read_to_string(&scenarios_path).map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to read {}: {}", scenarios_path.display(), e),
        ))
    })?;

    let scenarios_file: ScenariosFile = toml::from_str(&content).map_err(|e| {
        TaskError::Metadata(format!("Failed to parse behavior scenarios.toml: {}", e))
    })?;

    Ok(scenarios_file.scenario)
}

/// Filter scenarios to only include those that should be run.
pub fn filter_runnable(scenarios: Vec<BehaviorScenario>) -> Vec<BehaviorScenario> {
    scenarios.into_iter().filter(|s| !s.skip).collect()
}

/// Find a specific scenario by name.
#[allow(dead_code)]
pub fn find_scenario<'a>(
    scenarios: &'a [BehaviorScenario],
    name: &str,
) -> Option<&'a BehaviorScenario> {
    scenarios.iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_defaults() {
        let scenario = BehaviorScenario {
            name: "test".to_string(),
            description: "test desc".to_string(),
            args: vec![],
            setup: None,
            compare: vec![],
            skip: false,
            known_difference: None,
            cleanup: None,
        };

        // Empty compare list means compare everything by default
        assert!(scenario.compare_exit_code());
        assert!(scenario.compare_files());
        assert!(!scenario.compare_output());
        assert!(!scenario.compare_permissions());
    }

    #[test]
    fn test_compare_specific() {
        let scenario = BehaviorScenario {
            name: "test".to_string(),
            description: "test desc".to_string(),
            args: vec![],
            setup: None,
            compare: vec!["exit_code".to_string(), "symlinks".to_string()],
            skip: false,
            known_difference: None,
            cleanup: None,
        };

        assert!(scenario.compare_exit_code());
        assert!(!scenario.compare_files());
        assert!(scenario.compare_symlinks());
    }
}
