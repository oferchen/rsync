//! Behavior comparison testing for upstream rsync parity.
//!
//! This module provides infrastructure for running identical rsync operations
//! with both oc-rsync and upstream rsync, comparing their behavior including:
//! - Exit codes
//! - Resulting file states
//! - Output messages
//! - File metadata (permissions, timestamps, etc.)

pub mod comparison;
pub mod harness;
pub mod runner;
pub mod scenarios;

use crate::commands::interop::args::BehaviorOptions;
use crate::error::TaskResult;
use std::path::Path;

/// Execute behavior comparison tests.
pub fn execute(workspace: &Path, options: BehaviorOptions) -> TaskResult<()> {
    eprintln!("[behavior] Starting behavior comparison tests...");

    // Load scenarios
    let all_scenarios = scenarios::load_scenarios(workspace)?;
    let runnable = scenarios::filter_runnable(all_scenarios);

    eprintln!(
        "[behavior] Loaded {} runnable scenarios (from {} total)",
        runnable.len(),
        scenarios::load_scenarios(workspace)?.len()
    );

    // Create the test harness
    let harness = harness::BehaviorHarness::new(workspace, &options)?;

    eprintln!(
        "[behavior] Using oc-rsync: {}",
        harness.oc_rsync_path().display()
    );
    eprintln!(
        "[behavior] Using upstream rsync: {}",
        harness.upstream_rsync_path().display()
    );

    // Run comparison tests
    let results = harness.run_scenarios(&runnable)?;

    // Report results
    let passed = results.iter().filter(|r| r.passed()).count();
    let failed = results.len() - passed;

    eprintln!("\n=== Behavior Comparison Summary ===");
    eprintln!("Total scenarios: {}", results.len());
    eprintln!("Passed: {}", passed);
    eprintln!("Failed: {}", failed);

    if failed > 0 {
        eprintln!("\nFailed scenarios:");
        for result in results.iter().filter(|r| !r.passed()) {
            eprintln!("\n  {} - {}:", result.scenario_name, result.description);
            for diff in &result.differences {
                eprintln!("    - {}", diff);
            }
        }

        return Err(crate::error::TaskError::Validation(format!(
            "{} behavior comparison tests failed",
            failed
        )));
    }

    eprintln!("\n[behavior] All behavior comparison tests passed!");
    Ok(())
}
