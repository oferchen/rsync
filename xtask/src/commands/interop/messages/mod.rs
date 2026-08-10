//! Message format validation against upstream rsync.

mod extractor;
mod golden;
mod matcher;
mod normalizer;

use crate::commands::interop::args::MessagesOptions;
use crate::error::TaskResult;
use extractor::ExtractorOptions;
use std::path::Path;

/// Execute message format validation.
pub fn execute(workspace: &Path, options: MessagesOptions) -> TaskResult<()> {
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
        eprintln!("[interop] Regenerating message golden files...");
        regenerate_goldens(workspace, options)?;
    } else {
        eprintln!("[interop] Validating message formats against upstream rsync...");
        validate_messages(workspace, options)?;
    }

    Ok(())
}

/// Regenerate golden files for messages.
fn regenerate_goldens(workspace: &Path, options: MessagesOptions) -> TaskResult<()> {
    use super::shared::upstream;

    eprintln!("[interop] Message validation: Regenerate mode");
    eprintln!("[interop] Note: Message scenarios reuse the exit code scenario set");

    let scenarios_path = workspace.join("tests/interop/exit_codes/scenarios.toml");
    if !scenarios_path.exists() {
        return Err(crate::error::TaskError::Metadata(format!(
            "Scenarios file not found: {}\nRun exit code tests first to generate scenarios.",
            scenarios_path.display()
        )));
    }

    let scenarios = super::exit_codes::scenarios::load_scenarios(workspace)?;
    let runnable_scenarios = super::exit_codes::scenarios::filter_runnable(scenarios);

    eprintln!(
        "[interop] Loaded {} runnable scenarios",
        runnable_scenarios.len()
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
            "\n[interop] Generating golden messages for upstream rsync {}...",
            binary.version
        );

        let mut golden = golden::GoldenMessages::new(binary.version.clone());
        let mut total_messages = 0;

        for scenario in &runnable_scenarios {
            if options.verbose {
                eprintln!("[interop] Running scenario '{}'", scenario.name);
            }

            let msg_scenario = extractor::MessageScenario {
                name: scenario.name.clone(),
                args: scenario.args.clone(),
                setup: scenario.setup.clone(),
                description: scenario.description.clone(),
            };

            let extractor_opts = ExtractorOptions {
                verbose: options.verbose,
                show_output: options.show_output,
                log_dir: options.log_dir.clone(),
                version: Some(binary.version.clone()),
            };
            match msg_scenario.execute(binary.binary_path(), &extractor_opts) {
                Ok(messages) => {
                    let normalized = normalizer::normalize_messages(&messages);
                    for msg in &normalized {
                        golden.add_message(msg, &scenario.name);
                    }
                    total_messages += normalized.len();

                    if options.verbose && !normalized.is_empty() {
                        eprintln!(
                            "[interop]   Captured {} messages from '{}'",
                            normalized.len(),
                            scenario.name
                        );
                    }
                }
                Err(e) => {
                    if options.verbose {
                        eprintln!(
                            "[interop]   Warning: Failed to run '{}': {}",
                            scenario.name, e
                        );
                    }
                }
            }
        }

        golden::save_golden(workspace, &golden)?;
        eprintln!(
            "[interop] Generated golden file with {} messages from {} scenarios",
            total_messages,
            runnable_scenarios.len()
        );
    }

    eprintln!("\n[interop] Message golden file regeneration complete!");
    Ok(())
}

/// Validate messages against golden files.
fn validate_messages(workspace: &Path, options: MessagesOptions) -> TaskResult<()> {
    use super::shared::upstream;

    eprintln!("[interop] Message validation: Validate mode");
    eprintln!("[interop] Note: This validates message format stability against upstream");

    let scenarios = super::exit_codes::scenarios::load_scenarios(workspace)?;
    let runnable_scenarios = super::exit_codes::scenarios::filter_runnable(scenarios);

    eprintln!(
        "[interop] Loaded {} runnable scenarios",
        runnable_scenarios.len()
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
            "\n[interop] Validating messages against upstream rsync {}...",
            binary.version
        );

        let golden = golden::load_golden(workspace, &binary.version)?;

        let mut total_checked = 0;
        let mut total_differences = 0;

        for scenario in &runnable_scenarios {
            let msg_scenario = extractor::MessageScenario {
                name: scenario.name.clone(),
                args: scenario.args.clone(),
                setup: scenario.setup.clone(),
                description: scenario.description.clone(),
            };

            let extractor_opts = ExtractorOptions {
                verbose: options.verbose,
                show_output: options.show_output,
                log_dir: options.log_dir.clone(),
                version: Some(binary.version.clone()),
            };
            match msg_scenario.execute(binary.binary_path(), &extractor_opts) {
                Ok(messages) => {
                    let actual_normalized = normalizer::normalize_messages(&messages);

                    let actual_for_matcher: Vec<(String, Option<String>)> = actual_normalized
                        .iter()
                        .map(|m| (m.text.clone(), m.role.clone()))
                        .collect();

                    let (matchers, groups) = golden.get_matchers_for_scenario(&scenario.name);

                    let result =
                        matcher::validate_messages(&actual_for_matcher, &matchers, &groups);

                    total_checked += 1;

                    let differences = result.differences();
                    if !differences.is_empty() {
                        eprintln!(
                            "[interop]   Scenario '{}': {} differences",
                            scenario.name,
                            differences.len()
                        );
                        if options.verbose {
                            for diff in &differences {
                                eprintln!("[interop]     - {}", diff);
                            }
                        }
                        total_differences += differences.len();
                        validation_failed = true;
                    } else if options.verbose {
                        eprintln!("[interop]   Scenario '{}': OK", scenario.name);
                    }
                }
                Err(e) => {
                    if options.verbose {
                        eprintln!(
                            "[interop]   Warning: Failed to run '{}': {}",
                            scenario.name, e
                        );
                    }
                }
            }
        }

        if total_differences == 0 {
            eprintln!("[interop] ✓ All {} scenarios passed!", total_checked);
        } else {
            eprintln!(
                "[interop] ✗ {} differences found across {} scenarios",
                total_differences, total_checked
            );
        }
    }

    if validation_failed {
        Err(crate::error::TaskError::Validation(
            "Message validation failed".to_owned(),
        ))
    } else {
        eprintln!("\n[interop] Message validation complete - all tests passed!");
        Ok(())
    }
}
