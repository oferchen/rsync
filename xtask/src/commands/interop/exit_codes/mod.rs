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
    use super::shared::{oc_rsync, upstream};

    // Load test scenarios
    let all_scenarios = scenarios::load_scenarios(workspace)?;
    let runnable_scenarios = scenarios::filter_runnable(all_scenarios);

    eprintln!(
        "[interop] Loaded {} runnable scenarios",
        runnable_scenarios.len()
    );

    // Check which implementation to test
    let impl_name = options.implementation.as_deref().unwrap_or("upstream");

    match impl_name {
        "oc-rsync" => {
            // Test our oc-rsync implementation against all upstream golden files
            validate_oc_rsync(workspace, &runnable_scenarios, options)
        }
        "upstream" => {
            // Test upstream rsync (original behavior)
            validate_upstream(workspace, &runnable_scenarios, options)
        }
        other => Err(crate::error::TaskError::Usage(format!(
            "Unknown implementation '{}'. Use 'upstream' or 'oc-rsync'",
            other
        ))),
    }
}

/// Validate oc-rsync implementation against upstream golden files.
fn validate_oc_rsync(
    workspace: &Path,
    runnable_scenarios: &[scenarios::Scenario],
    options: ExitCodesOptions,
) -> TaskResult<()> {
    use super::shared::oc_rsync;

    eprintln!("[interop] Validating oc-rsync against upstream golden files...");

    // Detect oc-rsync binary
    let oc_binary = oc_rsync::detect_oc_rsync_binary(workspace)?;
    eprintln!("[interop] Found oc-rsync at: {}", oc_binary.binary_path().display());

    // We need to validate against all upstream versions' golden files
    let upstream_versions = super::shared::upstream::UPSTREAM_VERSIONS;
    let versions_to_test: Vec<_> = if let Some(ref version) = options.version {
        vec![version.as_str()]
    } else {
        upstream_versions.to_vec()
    };

    let mut validation_failed = false;

    for version in versions_to_test {
        eprintln!("\n[interop] Validating oc-rsync against upstream {} golden file...", version);

        // Load golden file for this upstream version
        let golden = golden::load_golden(workspace, version)?;

        // Run scenarios with oc-rsync
        let results = runner::run_scenarios(
            runnable_scenarios,
            oc_binary.binary_path(),
            options.verbose,
        )?;

        // Validate results against golden
        let errors = golden::validate_against_golden(&results, &golden);

        if errors.is_empty() {
            eprintln!("[interop] ✓ All {} scenarios passed for upstream {} baseline!", results.len(), version);
        } else {
            eprintln!("[interop] ✗ {} validation errors against upstream {} baseline:", errors.len(), version);
            for error in &errors {
                eprintln!("  - {}", error);
            }
            validation_failed = true;
        }
    }

    if validation_failed {
        Err(crate::error::TaskError::Validation(
            "oc-rsync exit code validation failed - does not match upstream behavior".to_string(),
        ))
    } else {
        eprintln!("\n[interop] oc-rsync validation complete - matches upstream behavior!");
        Ok(())
    }
}

/// Validate upstream rsync (original behavior).
fn validate_upstream(
    workspace: &Path,
    runnable_scenarios: &[scenarios::Scenario],
    options: ExitCodesOptions,
) -> TaskResult<()> {
    use super::shared::upstream;

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
            runner::run_scenarios(runnable_scenarios, binary.binary_path(), options.verbose)?;

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
