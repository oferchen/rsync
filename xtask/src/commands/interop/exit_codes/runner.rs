//! Scenario execution engine for exit code testing.

use super::scenarios::Scenario;
use crate::error::{TaskError, TaskResult};
use std::path::Path;
use std::process::Command;

/// Result of executing a single scenario.
#[derive(Debug, Clone)]
pub struct ScenarioResult {
    /// The scenario that was executed.
    pub scenario_name: String,
    /// The actual exit code returned by rsync.
    pub actual_exit_code: i32,
    /// The expected exit code from the scenario.
    #[allow(dead_code)]
    pub expected_exit_code: i32,
    /// Whether the exit code matched the expectation.
    #[allow(dead_code)]
    pub passed: bool,
}

/// Execute a scenario against a specific rsync binary.
pub fn run_scenario(
    scenario: &Scenario,
    rsync_binary: &Path,
    verbose: bool,
) -> TaskResult<ScenarioResult> {
    // Create a temporary directory for this test
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

    if verbose {
        eprintln!(
            "[runner] Executing scenario '{}' in {}",
            scenario.name,
            work_dir.display()
        );
    }

    // Run setup commands if specified
    if let Some(ref setup) = scenario.setup {
        if verbose {
            eprintln!("[runner] Running setup: {}", setup);
        }

        let setup_status = Command::new("bash")
            .arg("-c")
            .arg(setup)
            .current_dir(work_dir)
            .status()
            .map_err(|e| {
                TaskError::Io(std::io::Error::new(
                    e.kind(),
                    format!("Failed to run setup for '{}': {}", scenario.name, e),
                ))
            })?;

        if !setup_status.success() && verbose {
            eprintln!(
                "[runner] Warning: setup exited with code {:?}",
                setup_status.code()
            );
        }
    }

    // Execute rsync with the scenario arguments
    // Replace "rsync" in args with the actual binary path
    let mut cmd_args = scenario.args.clone();
    if !cmd_args.is_empty() && cmd_args[0] == "rsync" {
        cmd_args[0] = rsync_binary.to_string_lossy().to_string();
    }

    if verbose {
        eprintln!("[runner] Executing: {:?}", cmd_args);
    }

    let rsync_status = Command::new(&cmd_args[0])
        .args(&cmd_args[1..])
        .current_dir(work_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to execute rsync for '{}': {}", scenario.name, e),
            ))
        })?;

    let actual_exit_code = rsync_status.code().unwrap_or(-1);
    let passed = actual_exit_code == scenario.exit_code;

    if verbose {
        let status = if passed { "PASS" } else { "FAIL" };
        eprintln!(
            "[runner] {} - Expected: {}, Got: {}",
            status, scenario.exit_code, actual_exit_code
        );
    }

    Ok(ScenarioResult {
        scenario_name: scenario.name.clone(),
        actual_exit_code,
        expected_exit_code: scenario.exit_code,
        passed,
    })
}

/// Execute multiple scenarios and collect results.
pub fn run_scenarios(
    scenarios: &[Scenario],
    rsync_binary: &Path,
    verbose: bool,
) -> TaskResult<Vec<ScenarioResult>> {
    let mut results = Vec::new();

    for scenario in scenarios {
        let result = run_scenario(scenario, rsync_binary, verbose)?;
        results.push(result);
    }

    Ok(results)
}

/// Print a summary of scenario results.
#[allow(dead_code)]
pub fn print_summary(results: &[ScenarioResult]) {
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;

    eprintln!("\n=== Exit Code Validation Summary ===");
    eprintln!("Total scenarios: {}", results.len());
    eprintln!("Passed: {}", passed);
    eprintln!("Failed: {}", failed);

    if failed > 0 {
        eprintln!("\nFailed scenarios:");
        for result in results.iter().filter(|r| !r.passed) {
            eprintln!(
                "  - {}: expected {}, got {}",
                result.scenario_name, result.expected_exit_code, result.actual_exit_code
            );
        }
    }
}
