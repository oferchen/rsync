#![allow(clippy::module_name_repetitions)]

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

/// Result alias used by xtask helpers.
pub type TaskResult<T> = Result<T, TaskError>;

/// Errors raised by workspace maintenance commands.
#[derive(Debug)]
pub enum TaskError {
    /// Incorrect usage detected while parsing arguments.
    Usage(String),
    /// Help text requested by the caller.
    Help(String),
    /// I/O failure encountered while reading or writing files.
    Io(io::Error),
    /// Required external tooling was unavailable.
    ToolMissing(String),
    /// Metadata such as manifests or configuration files were invalid.
    Metadata(String),
    /// Validation failure encountered during consistency checks.
    Validation(String),
    /// Tracked binary artifacts were detected in the repository.
    BinaryFiles(Vec<PathBuf>),
    /// A subprocess exited unsuccessfully.
    CommandFailed {
        /// Program name used for diagnostics.
        program: String,
        /// Exit status returned by the program.
        status: ExitStatus,
    },
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Usage(message)
            | TaskError::Help(message)
            | TaskError::Validation(message)
            | TaskError::ToolMissing(message)
            | TaskError::Metadata(message) => f.write_str(message),
            TaskError::Io(error) => write!(f, "{error}"),
            TaskError::BinaryFiles(paths) => {
                writeln!(f, "binary files detected in repository:")?;
                for path in paths {
                    writeln!(f, "  {}", path.display())?;
                }
                Ok(())
            }
            TaskError::CommandFailed { program, status } => {
                if let Some(code) = status.code() {
                    write!(f, "{program} exited with status code {code}")
                } else {
                    write!(f, "{program} terminated by signal")
                }
            }
        }
    }
}

impl std::error::Error for TaskError {}

impl From<io::Error> for TaskError {
    fn from(error: io::Error) -> Self {
        TaskError::Io(error)
    }
}
