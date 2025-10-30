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

#[cfg(test)]
mod tests {
    use super::{TaskError, TaskResult};
    use std::io;
    use std::path::PathBuf;

    #[test]
    fn display_messages_match_variant_contents() {
        assert_eq!(TaskError::Usage("usage".into()).to_string(), "usage");
        assert_eq!(TaskError::Help("help".into()).to_string(), "help");
        assert_eq!(TaskError::Metadata("meta".into()).to_string(), "meta");
        assert_eq!(
            TaskError::Validation("invalid".into()).to_string(),
            "invalid"
        );
        assert_eq!(TaskError::ToolMissing("tool".into()).to_string(), "tool");

        let io_error = TaskError::from(io::Error::new(io::ErrorKind::Other, "io"));
        assert_eq!(io_error.to_string(), "io");

        let binary = TaskError::BinaryFiles(vec![PathBuf::from("bin/artifact")]).to_string();
        assert!(binary.contains("binary files detected"));
        assert!(binary.contains("bin/artifact"));

        #[cfg(unix)]
        {
            let status = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("exit 3")
                .status()
                .expect("shell exit");
            let message = TaskError::CommandFailed {
                program: "sh".into(),
                status,
            }
            .to_string();
            assert!(message.contains("status code 3"));
        }
    }

    #[test]
    fn task_result_type_alias_is_convenient() {
        fn helper() -> TaskResult<()> {
            Err(TaskError::Usage("bad".into()))
        }

        assert!(matches!(helper(), Err(TaskError::Usage(message)) if message == "bad"));
    }
}
