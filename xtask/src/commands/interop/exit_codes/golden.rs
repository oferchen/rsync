//! Golden file management for exit codes.

use super::runner::ScenarioResult;
use crate::error::{TaskError, TaskResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Golden file data structure: maps scenario names to expected exit codes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenData {
    /// Version of upstream rsync this golden file represents.
    pub version: String,
    /// Map of scenario name -> exit code.
    pub exit_codes: HashMap<String, i32>,
}

impl GoldenData {
    /// Create a new golden data structure for a specific version.
    pub fn new(version: String) -> Self {
        Self {
            version,
            exit_codes: HashMap::new(),
        }
    }

    /// Add a scenario result to the golden data.
    pub fn add_result(&mut self, result: &ScenarioResult) {
        self.exit_codes
            .insert(result.scenario_name.clone(), result.actual_exit_code);
    }

    /// Get the expected exit code for a scenario.
    pub fn get_exit_code(&self, scenario_name: &str) -> Option<i32> {
        self.exit_codes.get(scenario_name).copied()
    }
}

/// Get the path to a golden file for a specific upstream version.
pub fn golden_file_path(workspace: &Path, version: &str) -> PathBuf {
    workspace.join(format!("tests/interop/exit_codes/golden-{}.toml", version))
}

/// Load golden data from a file.
pub fn load_golden(workspace: &Path, version: &str) -> TaskResult<GoldenData> {
    let path = golden_file_path(workspace, version);

    if !path.exists() {
        return Err(TaskError::Metadata(format!(
            "Golden file not found for version {}: {}\nRun with --regenerate to create it.",
            version,
            path.display()
        )));
    }

    let content = std::fs::read_to_string(&path).map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to read golden file {}: {}", path.display(), e),
        ))
    })?;

    let golden: GoldenData = toml::from_str(&content).map_err(|e| {
        TaskError::Metadata(format!(
            "Failed to parse golden file {}: {}",
            path.display(),
            e
        ))
    })?;

    Ok(golden)
}

/// Save golden data to a file.
pub fn save_golden(workspace: &Path, golden: &GoldenData) -> TaskResult<()> {
    let path = golden_file_path(workspace, &golden.version);

    // Ensure the directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = toml::to_string_pretty(golden)
        .map_err(|e| TaskError::Metadata(format!("Failed to serialize golden data: {}", e)))?;

    std::fs::write(&path, content).map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to write golden file {}: {}", path.display(), e),
        ))
    })?;

    eprintln!("[golden] Wrote golden file: {}", path.display());
    Ok(())
}

/// Generate golden data from scenario results.
pub fn generate_golden(version: String, results: &[ScenarioResult]) -> GoldenData {
    let mut golden = GoldenData::new(version);

    for result in results {
        golden.add_result(result);
    }

    golden
}

/// Validate scenario results against golden data.
pub fn validate_against_golden(results: &[ScenarioResult], golden: &GoldenData) -> Vec<String> {
    let mut errors = Vec::new();

    for result in results {
        match golden.get_exit_code(&result.scenario_name) {
            Some(expected) => {
                if result.actual_exit_code != expected {
                    errors.push(format!(
                        "Scenario '{}': expected exit code {}, got {}",
                        result.scenario_name, expected, result.actual_exit_code
                    ));
                }
            }
            None => {
                errors.push(format!(
                    "Scenario '{}' not found in golden file for version {}",
                    result.scenario_name, golden.version
                ));
            }
        }
    }

    errors
}
