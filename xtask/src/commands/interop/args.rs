//! CLI argument parsing for interop validation commands.

use crate::cli::{InteropArgs, InteropCommand as CliInteropCommand, InteropCommonArgs};

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
    /// Implementation to test: "upstream" (default) or "oc-rsync".
    pub implementation: Option<String>,
    /// Show stdout/stderr output from rsync commands.
    pub show_output: bool,
    /// Directory to save rsync logs (uses --log-file).
    pub log_dir: Option<String>,
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
    /// Implementation to test: "upstream" (default) or "oc-rsync".
    /// TODO: Currently only upstream is supported for message validation
    #[allow(dead_code)]
    pub implementation: Option<String>,
    /// Show stdout/stderr output from rsync commands.
    pub show_output: bool,
    /// Directory to save rsync logs (uses --log-file).
    pub log_dir: Option<String>,
}

impl From<InteropCommonArgs> for ExitCodesOptions {
    fn from(args: InteropCommonArgs) -> Self {
        Self {
            regenerate: args.regenerate,
            version: args.version,
            verbose: args.verbose,
            implementation: args.implementation,
            show_output: args.show_output,
            log_dir: args.log_dir,
        }
    }
}

impl From<InteropCommonArgs> for MessagesOptions {
    fn from(args: InteropCommonArgs) -> Self {
        Self {
            regenerate: args.regenerate,
            version: args.version,
            verbose: args.verbose,
            implementation: args.implementation,
            show_output: args.show_output,
            log_dir: args.log_dir,
        }
    }
}

impl From<InteropArgs> for InteropOptions {
    fn from(args: InteropArgs) -> Self {
        let command = args.command.unwrap_or(CliInteropCommand::All);
        let command = match command {
            CliInteropCommand::ExitCodes(common) => InteropCommand::ExitCodes(common.into()),
            CliInteropCommand::Messages(common) => InteropCommand::Messages(common.into()),
            CliInteropCommand::All => InteropCommand::All,
        };
        Self { command }
    }
}
