//! Interoperability validation commands for testing against upstream rsync.
//!
//! This module provides subcommands to validate exit codes and message formats
//! against upstream rsync versions (3.0.9, 3.1.3, 3.4.1).

#![allow(clippy::uninlined_format_args)]

mod args;
mod exit_codes;
mod messages;
mod shared;

use crate::error::TaskResult;
use std::path::Path;

pub use args::{InteropCommand, InteropOptions};

/// Execute the interop validation command.
pub fn execute(workspace: &Path, options: InteropOptions) -> TaskResult<()> {
    match options.command {
        InteropCommand::ExitCodes(opts) => {
            exit_codes::execute(workspace, opts)?;
        }
        InteropCommand::Messages(opts) => {
            messages::execute(workspace, opts)?;
        }
        InteropCommand::All => {
            // Run both exit codes and messages validation
            eprintln!("Running exit code validation...");
            exit_codes::execute(workspace, args::ExitCodesOptions::default())?;

            eprintln!("\nRunning message format validation...");
            messages::execute(workspace, args::MessagesOptions::default())?;
        }
    }

    Ok(())
}
