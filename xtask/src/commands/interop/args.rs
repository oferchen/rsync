//! CLI argument parsing for interop validation commands.

use crate::error::{TaskError, TaskResult};
use std::ffi::OsString;

/// Options for the interop command.
#[derive(Debug, Clone)]
pub struct InteropOptions {
    /// The subcommand to execute.
    pub command: InteropCommand,
}

/// Interop subcommands.
#[derive(Debug, Clone)]
pub enum InteropCommand {
    /// Validate exit codes against upstream rsync.
    ExitCodes(ExitCodesOptions),
    /// Validate message formats against upstream rsync.
    Messages(MessagesOptions),
    /// Run all validation (exit codes + messages).
    All,
}

/// Options for exit code validation.
#[derive(Debug, Clone, Default)]
pub struct ExitCodesOptions {
    /// Regenerate golden files instead of validating.
    pub regenerate: bool,
    /// Specific upstream version to test (default: all).
    pub version: Option<String>,
    /// Enable verbose output.
    pub verbose: bool,
}

/// Options for message format validation.
#[derive(Debug, Clone, Default)]
pub struct MessagesOptions {
    /// Regenerate golden files instead of validating.
    pub regenerate: bool,
    /// Specific upstream version to test (default: all).
    pub version: Option<String>,
    /// Enable verbose output.
    pub verbose: bool,
}

/// Parse interop command arguments.
pub fn parse<I>(args: I) -> TaskResult<InteropOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();

    if args.is_empty() {
        // Default to running all validations
        return Ok(InteropOptions {
            command: InteropCommand::All,
        });
    }

    let subcommand = &args[0];
    let remaining = &args[1..];

    let command = match subcommand.to_string_lossy().as_ref() {
        "exit-codes" => {
            let opts = parse_common_options(remaining)?;
            InteropCommand::ExitCodes(ExitCodesOptions {
                regenerate: opts.regenerate,
                version: opts.version,
                verbose: opts.verbose,
            })
        }
        "messages" => {
            let opts = parse_common_options(remaining)?;
            InteropCommand::Messages(MessagesOptions {
                regenerate: opts.regenerate,
                version: opts.version,
                verbose: opts.verbose,
            })
        }
        "all" => InteropCommand::All,
        "--help" | "-h" => {
            return Err(TaskError::Help(usage()));
        }
        other => {
            return Err(TaskError::Usage(format!("Unknown subcommand: {}", other)));
        }
    };

    Ok(InteropOptions { command })
}

/// Common options shared by exit-codes and messages subcommands.
struct CommonOptions {
    regenerate: bool,
    version: Option<String>,
    verbose: bool,
}

/// Parse common options (--regenerate, --version, --verbose).
fn parse_common_options(args: &[OsString]) -> TaskResult<CommonOptions> {
    let mut regenerate = false;
    let mut version = None;
    let mut verbose = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].to_string_lossy().as_ref() {
            "--regenerate" => {
                regenerate = true;
                i += 1;
            }
            "--version" => {
                if i + 1 >= args.len() {
                    return Err(TaskError::Usage(String::from(
                        "--version requires an argument",
                    )));
                }
                version = Some(args[i + 1].to_string_lossy().into_owned());
                i += 2;
            }
            "--verbose" | "-v" => {
                verbose = true;
                i += 1;
            }
            "--help" | "-h" => {
                return Err(TaskError::Help(usage()));
            }
            other => {
                return Err(TaskError::Usage(format!("Unknown option: {}", other)));
            }
        }
    }

    Ok(CommonOptions {
        regenerate,
        version,
        verbose,
    })
}

/// Return usage information.
pub fn usage() -> String {
    r#"
USAGE:
    cargo xtask interop [SUBCOMMAND] [OPTIONS]

SUBCOMMANDS:
    exit-codes    Validate exit codes against upstream rsync
    messages      Validate message formats against upstream rsync
    all           Run all validations (default)

OPTIONS:
    --regenerate     Regenerate golden files instead of validating
    --version VER    Test against specific upstream version (3.0.9, 3.1.3, 3.4.1)
    --verbose, -v    Enable verbose output
    --help, -h       Show this help message

EXAMPLES:
    # Validate exit codes against all upstream versions
    cargo xtask interop exit-codes

    # Regenerate golden files for exit codes
    cargo xtask interop exit-codes --regenerate

    # Validate messages for specific upstream version
    cargo xtask interop messages --version 3.4.1

    # Run all validations (exit codes + messages)
    cargo xtask interop all
"#
    .to_string()
}
