//! Behavior comparison test harness.
//!
//! This module provides the main harness for running behavior comparison tests
//! between oc-rsync and upstream rsync.

use super::comparison::{Difference, compare_results};
use super::runner::{RunOptions, RunResult, cleanup_scenario, run_scenario, setup_scenario};
use super::scenarios::BehaviorScenario;
use crate::commands::interop::args::BehaviorOptions;
use crate::commands::interop::shared::{oc_rsync, upstream};
use crate::error::{TaskError, TaskResult};
use std::path::{Path, PathBuf};

/// Result of running a behavior comparison test.
#[derive(Debug)]
pub struct ComparisonResult {
    /// Scenario name.
    pub scenario_name: String,

    /// Scenario description.
    pub description: String,

    /// Differences found between implementations.
    pub differences: Vec<Difference>,

    /// oc-rsync run result (for debugging).
    #[allow(dead_code)]
    pub oc_rsync_result: RunResult,

    /// Upstream rsync run result (for debugging).
    #[allow(dead_code)]
    pub upstream_result: RunResult,
}

impl ComparisonResult {
    /// Check if the comparison passed (no differences).
    pub fn passed(&self) -> bool {
        self.differences.is_empty()
    }
}

/// The main behavior test harness.
pub struct BehaviorHarness {
    /// Path to oc-rsync binary.
    oc_rsync_binary: PathBuf,

    /// Path to upstream rsync binary.
    upstream_binary: PathBuf,

    /// Workspace root (kept for potential future use).
    #[allow(dead_code)]
    workspace: PathBuf,

    /// Test options.
    options: BehaviorTestOptions,
}

/// Options for behavior testing.
#[derive(Debug, Clone, Default)]
pub struct BehaviorTestOptions {
    /// Enable verbose output.
    pub verbose: bool,

    /// Show stdout/stderr from commands.
    pub show_output: bool,

    /// Specific scenario name to run (None = run all).
    pub scenario: Option<String>,

    /// Stop on first failure.
    pub fail_fast: bool,
}

impl BehaviorHarness {
    /// Create a new behavior test harness.
    pub fn new(workspace: &Path, options: &BehaviorOptions) -> TaskResult<Self> {
        // Detect oc-rsync binary
        let oc_rsync_binary = oc_rsync::detect_oc_rsync_binary(workspace)?;

        // Detect upstream rsync binary
        let upstream_binaries = upstream::detect_upstream_binaries(workspace)?;

        // Use specified version or latest available
        let upstream_binary = if let Some(ref version) = options.version {
            upstream_binaries
                .iter()
                .find(|b| b.version == *version)
                .ok_or_else(|| {
                    TaskError::ToolMissing(format!("Upstream rsync version {} not found", version))
                })?
                .path
                .clone()
        } else {
            // Use latest version (last in list, which is 3.4.1)
            upstream_binaries
                .last()
                .ok_or_else(|| {
                    TaskError::ToolMissing("No upstream rsync binaries found".to_string())
                })?
                .path
                .clone()
        };

        let test_options = BehaviorTestOptions {
            verbose: options.verbose,
            show_output: options.show_output,
            scenario: options.scenario.clone(),
            fail_fast: options.fail_fast,
        };

        Ok(Self {
            oc_rsync_binary: oc_rsync_binary.path,
            upstream_binary,
            workspace: workspace.to_path_buf(),
            options: test_options,
        })
    }

    /// Get path to oc-rsync binary.
    pub fn oc_rsync_path(&self) -> &Path {
        &self.oc_rsync_binary
    }

    /// Get path to upstream rsync binary.
    pub fn upstream_rsync_path(&self) -> &Path {
        &self.upstream_binary
    }

    /// Run a single scenario and compare results.
    pub fn run_scenario(&self, scenario: &BehaviorScenario) -> TaskResult<ComparisonResult> {
        // Create temporary work directory
        let temp_dir = tempfile::tempdir().map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to create temp dir for scenario '{}': {}",
                    scenario.name, e
                ),
            ))
        })?;

        let work_dir = temp_dir.path();

        if self.options.verbose {
            eprintln!(
                "[harness] Running scenario '{}' in {}",
                scenario.name,
                work_dir.display()
            );
        }

        let run_options = RunOptions {
            verbose: self.options.verbose,
            show_output: self.options.show_output,
        };

        // Set up test environment (shared for both runs)
        setup_scenario(scenario, work_dir, &run_options)?;

        // Run with upstream rsync first (to a clean dest directory)
        let upstream_result = run_scenario(
            scenario,
            &self.upstream_binary,
            work_dir,
            "dest_upstream",
            &run_options,
        )?;

        // Set up fresh environment for oc-rsync
        // (We need to re-run setup because dest may have been modified)
        setup_scenario(scenario, work_dir, &run_options)?;

        // Run with oc-rsync
        let oc_rsync_result = run_scenario(
            scenario,
            &self.oc_rsync_binary,
            work_dir,
            "dest_oc",
            &run_options,
        )?;

        // Compare results
        let differences = compare_results(scenario, &oc_rsync_result, &upstream_result);

        // Cleanup
        let _ = cleanup_scenario(scenario, work_dir, &run_options);

        Ok(ComparisonResult {
            scenario_name: scenario.name.clone(),
            description: scenario.description.clone(),
            differences,
            oc_rsync_result,
            upstream_result,
        })
    }

    /// Run multiple scenarios and collect results.
    pub fn run_scenarios(
        &self,
        scenarios: &[BehaviorScenario],
    ) -> TaskResult<Vec<ComparisonResult>> {
        let mut results = Vec::new();

        // Filter by specific scenario if requested
        let scenarios_to_run: Vec<_> = if let Some(ref name) = self.options.scenario {
            scenarios.iter().filter(|s| s.name == *name).collect()
        } else {
            scenarios.iter().collect()
        };

        if scenarios_to_run.is_empty() {
            if let Some(ref name) = self.options.scenario {
                return Err(TaskError::Usage(format!("Scenario '{}' not found", name)));
            }
        }

        for (idx, scenario) in scenarios_to_run.iter().enumerate() {
            if self.options.verbose {
                eprintln!(
                    "\n[harness] [{}/{}] Running: {} - {}",
                    idx + 1,
                    scenarios_to_run.len(),
                    scenario.name,
                    scenario.description
                );
            } else {
                eprint!(".");
            }

            let result = self.run_scenario(scenario)?;

            let passed = result.passed();
            results.push(result);

            if !passed && self.options.fail_fast {
                eprintln!("\n[harness] Stopping on first failure (--fail-fast)");
                break;
            }
        }

        if !self.options.verbose {
            eprintln!(); // Newline after progress dots
        }

        Ok(results)
    }
}

/// Builder for creating behavior test harnesses with custom configuration.
///
/// This allows programmatic configuration of the test harness for use in
/// custom test runners or CI pipelines.
#[allow(dead_code)]
pub struct BehaviorHarnessBuilder {
    oc_rsync_binary: Option<PathBuf>,
    upstream_binary: Option<PathBuf>,
    workspace: PathBuf,
    options: BehaviorTestOptions,
}

#[allow(dead_code)]
impl BehaviorHarnessBuilder {
    /// Create a new builder.
    pub fn new(workspace: &Path) -> Self {
        Self {
            oc_rsync_binary: None,
            upstream_binary: None,
            workspace: workspace.to_path_buf(),
            options: BehaviorTestOptions::default(),
        }
    }

    /// Set the oc-rsync binary path.
    pub fn oc_rsync_binary(mut self, path: PathBuf) -> Self {
        self.oc_rsync_binary = Some(path);
        self
    }

    /// Set the upstream rsync binary path.
    pub fn upstream_binary(mut self, path: PathBuf) -> Self {
        self.upstream_binary = Some(path);
        self
    }

    /// Enable verbose output.
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.options.verbose = verbose;
        self
    }

    /// Enable showing command output.
    pub fn show_output(mut self, show_output: bool) -> Self {
        self.options.show_output = show_output;
        self
    }

    /// Run only a specific scenario.
    pub fn scenario(mut self, name: String) -> Self {
        self.options.scenario = Some(name);
        self
    }

    /// Stop on first failure.
    pub fn fail_fast(mut self, fail_fast: bool) -> Self {
        self.options.fail_fast = fail_fast;
        self
    }

    /// Build the harness.
    pub fn build(self) -> TaskResult<BehaviorHarness> {
        let oc_rsync_binary = self
            .oc_rsync_binary
            .or_else(|| {
                oc_rsync::detect_oc_rsync_binary(&self.workspace)
                    .ok()
                    .map(|b| b.path)
            })
            .ok_or_else(|| TaskError::ToolMissing("oc-rsync binary not found".to_string()))?;

        let upstream_binary = self
            .upstream_binary
            .or_else(|| {
                upstream::detect_upstream_binaries(&self.workspace)
                    .ok()
                    .and_then(|binaries| binaries.last().map(|b| b.path.clone()))
            })
            .ok_or_else(|| TaskError::ToolMissing("Upstream rsync binary not found".to_string()))?;

        Ok(BehaviorHarness {
            oc_rsync_binary,
            upstream_binary,
            workspace: self.workspace,
            options: self.options,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comparison_result_passed() {
        use super::super::runner::FileState;

        let result = ComparisonResult {
            scenario_name: "test".to_string(),
            description: "test desc".to_string(),
            differences: vec![],
            oc_rsync_result: RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                files: FileState::default(),
            },
            upstream_result: RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                files: FileState::default(),
            },
        };

        assert!(result.passed());
    }
}
