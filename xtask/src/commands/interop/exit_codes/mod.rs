//! Exit code validation against upstream rsync.

mod golden;
mod runner;
pub mod scenarios;

use crate::commands::interop::args::ExitCodesOptions;
use crate::error::TaskResult;
use std::path::Path;

/// Execute exit code validation.
pub fn execute(workspace: &Path, options: ExitCodesOptions) -> TaskResult<()> {
    if options.regenerate {
        eprintln!("[interop] Regenerating exit code golden files...");
        regenerate_goldens(workspace, options)?;
    } else {
        eprintln!("[interop] Validating exit codes against upstream rsync...");
        validate_exit_codes(workspace, options)?;
    }

    Ok(())
}

/// Regenerate golden files for exit codes.
fn regenerate_goldens(workspace: &Path, options: ExitCodesOptions) -> TaskResult<()> {
    use super::shared::upstream;

    // Load test scenarios
    let all_scenarios = scenarios::load_scenarios(workspace)?;
    let runnable_scenarios = scenarios::filter_runnable(all_scenarios);

    eprintln!(
        "[interop] Loaded {} runnable scenarios (from {} total)",
        runnable_scenarios.len(),
        scenarios::load_scenarios(workspace)?.len()
    );

    // Detect upstream binaries
    let upstream_binaries = upstream::detect_upstream_binaries(workspace)?;

    // Filter by version if specified
    let binaries_to_test: Vec<_> = if let Some(ref version) = options.version {
        upstream_binaries
            .into_iter()
            .filter(|b| b.version == *version)
            .collect()
    } else {
        upstream_binaries
    };

    if binaries_to_test.is_empty() {
        if let Some(ref v) = options.version {
            return Err(crate::error::TaskError::ToolMissing(format!(
                "Upstream rsync version {} not found",
                v
            )));
        } else {
            return Err(crate::error::TaskError::ToolMissing(
                "No upstream rsync binaries found".to_string(),
            ));
        }
    }

    // Run scenarios against each upstream version and generate golden files
    for binary in &binaries_to_test {
        eprintln!(
            "\n[interop] Generating golden file for upstream rsync {}...",
            binary.version
        );

        let results =
            runner::run_scenarios(&runnable_scenarios, binary.binary_path(), options.verbose)?;

        // Generate and save golden file
        let golden = golden::generate_golden(binary.version.clone(), &results);
        golden::save_golden(workspace, &golden)?;

        eprintln!(
            "[interop] Generated golden file for {} scenarios",
            results.len()
        );
    }

    eprintln!("\n[interop] Golden file regeneration complete!");
    Ok(())
}

/// Validate exit codes against golden files.
fn validate_exit_codes(workspace: &Path, options: ExitCodesOptions) -> TaskResult<()> {
    use super::shared::upstream;

    // Load test scenarios
    let all_scenarios = scenarios::load_scenarios(workspace)?;
    let runnable_scenarios = scenarios::filter_runnable(all_scenarios);

    eprintln!(
        "[interop] Loaded {} runnable scenarios",
        runnable_scenarios.len()
    );

    // Detect upstream binaries
    let upstream_binaries = upstream::detect_upstream_binaries(workspace)?;

    // Filter by version if specified
    let binaries_to_test: Vec<_> = if let Some(ref version) = options.version {
        upstream_binaries
            .into_iter()
            .filter(|b| b.version == *version)
            .collect()
    } else {
        upstream_binaries
    };

    if binaries_to_test.is_empty() {
        if let Some(ref v) = options.version {
            return Err(crate::error::TaskError::ToolMissing(format!(
                "Upstream rsync version {} not found",
                v
            )));
        }
    }

    let mut validation_failed = false;

    // Validate against each upstream version
    for binary in &binaries_to_test {
        eprintln!(
            "\n[interop] Validating against upstream rsync {}...",
            binary.version
        );

        // Load golden file
        let golden = golden::load_golden(workspace, &binary.version)?;

        // Run scenarios
        let results =
            runner::run_scenarios(&runnable_scenarios, binary.binary_path(), options.verbose)?;

        // Validate results
        let errors = golden::validate_against_golden(&results, &golden);

        if errors.is_empty() {
            eprintln!("[interop] ✓ All {} scenarios passed!", results.len());
        } else {
            eprintln!("[interop] ✗ {} validation errors:", errors.len());
            for error in &errors {
                eprintln!("  - {}", error);
            }
            validation_failed = true;
        }
    }

    if validation_failed {
        Err(crate::error::TaskError::Validation(
            "Exit code validation failed".to_string(),
        ))
    } else {
        eprintln!("\n[interop] Exit code validation complete - all tests passed!");
        Ok(())
    }
}
