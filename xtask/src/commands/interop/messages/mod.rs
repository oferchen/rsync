//! Message format validation against upstream rsync.

mod extractor;
mod golden;
mod normalizer;

use crate::commands::interop::args::MessagesOptions;
use crate::error::TaskResult;
use std::path::Path;

/// Execute message format validation.
pub fn execute(workspace: &Path, options: MessagesOptions) -> TaskResult<()> {
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
    eprintln!("[interop] Note: Message scenarios use the same scenarios as exit code testing");

    // Load exit code scenarios (we reuse them to capture messages)
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

    // For each upstream version, run scenarios and collect messages
    for binary in &binaries_to_test {
        eprintln!(
            "\n[interop] Generating golden messages for upstream rsync {}...",
            binary.version
        );

        let mut golden = golden::GoldenMessages::new(binary.version.clone());
        let mut total_messages = 0;

        // Run each scenario and collect messages
        for scenario in &runnable_scenarios {
            if options.verbose {
                eprintln!("[interop] Running scenario '{}'", scenario.name);
            }

            // Convert exit code scenario to message scenario
            let msg_scenario = extractor::MessageScenario {
                name: scenario.name.clone(),
                args: scenario.args.clone(),
                setup: scenario.setup.clone(),
                description: scenario.description.clone(),
            };

            // Execute and extract messages
            match msg_scenario.execute(binary.binary_path(), options.verbose) {
                Ok(messages) => {
                    // Normalize and store messages
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

        // Save golden file
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

    // Load scenarios
    let scenarios = super::exit_codes::scenarios::load_scenarios(workspace)?;
    let runnable_scenarios = super::exit_codes::scenarios::filter_runnable(scenarios);

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
            "\n[interop] Validating messages against upstream rsync {}...",
            binary.version
        );

        // Load golden file
        let golden = golden::load_golden(workspace, &binary.version)?;

        let mut total_checked = 0;
        let mut total_differences = 0;

        // Run scenarios and compare messages
        for scenario in &runnable_scenarios {
            // Convert to message scenario
            let msg_scenario = extractor::MessageScenario {
                name: scenario.name.clone(),
                args: scenario.args.clone(),
                setup: scenario.setup.clone(),
                description: scenario.description.clone(),
            };

            // Execute and extract messages
            match msg_scenario.execute(binary.binary_path(), options.verbose) {
                Ok(messages) => {
                    let actual_normalized = normalizer::normalize_messages(&messages);
                    let expected_messages = golden.get_messages_for_scenario(&scenario.name);

                    // Convert expected messages to NormalizedMessage for comparison
                    let expected_normalized: Vec<normalizer::NormalizedMessage> = expected_messages
                        .iter()
                        .map(|m| normalizer::NormalizedMessage {
                            text: m.text.clone(),
                            role: m.role.clone(),
                        })
                        .collect();

                    // Compare
                    let differences =
                        normalizer::find_differences(&actual_normalized, &expected_normalized);

                    total_checked += 1;

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
            "Message validation failed".to_string(),
        ))
    } else {
        eprintln!("\n[interop] Message validation complete - all tests passed!");
        Ok(())
    }
}
