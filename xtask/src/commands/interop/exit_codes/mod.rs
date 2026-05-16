//! Exit code validation against upstream rsync.

mod golden;
mod runner;
pub mod scenarios;

use crate::commands::interop::args::ExitCodesOptions;
use crate::error::TaskResult;
use runner::RunnerOptions;
use std::path::Path;

/// Execute exit code validation.
pub fn execute(workspace: &Path, options: ExitCodesOptions) -> TaskResult<()> {
    if let Some(ref log_dir) = options.log_dir {
        std::fs::create_dir_all(log_dir).map_err(|e| {
            crate::error::TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to create log directory '{}': {}", log_dir, e),
            ))
        })?;
        eprintln!("[interop] Logs will be saved to: {}", log_dir);
    }

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

    let all_scenarios = scenarios::load_scenarios(workspace)?;
    let runnable_scenarios = scenarios::filter_runnable(all_scenarios);

    eprintln!(
        "[interop] Loaded {} runnable scenarios (from {} total)",
        runnable_scenarios.len(),
        scenarios::load_scenarios(workspace)?.len()
    );

    let upstream_binaries = upstream::detect_upstream_binaries(workspace)?;

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
                "No upstream rsync binaries found".to_owned(),
            ));
        }
    }

    for binary in &binaries_to_test {
        eprintln!(
            "\n[interop] Generating golden file for upstream rsync {}...",
            binary.version
        );

        let runner_opts = RunnerOptions {
            verbose: options.verbose,
            show_output: options.show_output,
            log_dir: options.log_dir.clone(),
            version: Some(binary.version.clone()),
        };
        let results =
            runner::run_scenarios(&runnable_scenarios, binary.binary_path(), &runner_opts)?;

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
    let all_scenarios = scenarios::load_scenarios(workspace)?;
    let runnable_scenarios = scenarios::filter_runnable(all_scenarios);

    eprintln!(
        "[interop] Loaded {} runnable scenarios",
        runnable_scenarios.len()
    );

    let impl_name = options.implementation.as_deref().unwrap_or("upstream");

    match impl_name {
        "oc-rsync" => validate_oc_rsync(workspace, &runnable_scenarios, options),
        "upstream" => validate_upstream(workspace, &runnable_scenarios, options),
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

    let oc_binary = oc_rsync::detect_oc_rsync_binary(workspace)?;
    eprintln!(
        "[interop] Found oc-rsync at: {}",
        oc_binary.binary_path().display()
    );

    let upstream_versions = super::shared::upstream::UPSTREAM_VERSIONS;
    let versions_to_test: Vec<_> = if let Some(ref version) = options.version {
        vec![version.as_str()]
    } else {
        upstream_versions.to_vec()
    };

    let mut validation_failed = false;

    for version in versions_to_test {
        eprintln!(
            "\n[interop] Validating oc-rsync against upstream {} golden file...",
            version
        );

        let golden = golden::load_golden(workspace, version)?;

        let runner_opts = RunnerOptions {
            verbose: options.verbose,
            show_output: options.show_output,
            log_dir: options.log_dir.clone(),
            version: Some(version.to_owned()),
        };
        let results =
            runner::run_scenarios(runnable_scenarios, oc_binary.binary_path(), &runner_opts)?;

        let errors = golden::validate_against_golden(&results, &golden);

        if errors.is_empty() {
            eprintln!(
                "[interop] ✓ All {} scenarios passed for upstream {} baseline!",
                results.len(),
                version
            );
        } else {
            eprintln!(
                "[interop] ✗ {} validation errors against upstream {} baseline:",
                errors.len(),
                version
            );
            for error in &errors {
                eprintln!("  - {}", error);
            }
            validation_failed = true;
        }
    }

    if validation_failed {
        Err(crate::error::TaskError::Validation(
            "oc-rsync exit code validation failed - does not match upstream behavior".to_owned(),
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

    let upstream_binaries = upstream::detect_upstream_binaries(workspace)?;

    let binaries_to_test: Vec<_> = if let Some(ref version) = options.version {
        upstream_binaries
            .into_iter()
            .filter(|b| b.version == *version)
            .collect()
    } else {
        upstream_binaries
    };

    if binaries_to_test.is_empty()
        && let Some(ref v) = options.version
    {
        return Err(crate::error::TaskError::ToolMissing(format!(
            "Upstream rsync version {} not found",
            v
        )));
    }

    let mut validation_failed = false;

    for binary in &binaries_to_test {
        eprintln!(
            "\n[interop] Validating against upstream rsync {}...",
            binary.version
        );

        let golden = golden::load_golden(workspace, &binary.version)?;

        let runner_opts = RunnerOptions {
            verbose: options.verbose,
            show_output: options.show_output,
            log_dir: options.log_dir.clone(),
            version: Some(binary.version.clone()),
        };
        let results =
            runner::run_scenarios(runnable_scenarios, binary.binary_path(), &runner_opts)?;

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
            "Exit code validation failed".to_owned(),
        ))
    } else {
        eprintln!("\n[interop] Exit code validation complete - all tests passed!");
        Ok(())
    }
}
