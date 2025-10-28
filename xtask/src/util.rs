use crate::error::{TaskError, TaskResult};
use serde_json::Value as JsonValue;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::process::Command;

/// Returns `true` when the argument requests help output.
pub fn is_help_flag(value: &OsString) -> bool {
    value == "--help" || value == "-h"
}

/// Ensures the provided condition holds, returning a [`TaskError::Validation`]
/// otherwise.
pub fn ensure(condition: bool, message: impl Into<String>) -> TaskResult<()> {
    if condition {
        Ok(())
    } else {
        Err(validation_error(message))
    }
}

/// Constructs a [`TaskError::Validation`] using the provided message.
pub fn validation_error(message: impl Into<String>) -> TaskError {
    TaskError::Validation(message.into())
}

fn map_command_error(error: io::Error, program: &str, install_hint: &str) -> TaskError {
    if error.kind() == io::ErrorKind::NotFound {
        TaskError::ToolMissing(format!("{program} is unavailable; {install_hint}"))
    } else {
        TaskError::Io(error)
    }
}

/// Runs `cargo` with the supplied arguments and maps failures to [`TaskError`].
pub fn run_cargo_tool(
    workspace: &std::path::Path,
    args: Vec<OsString>,
    display: &str,
    install_hint: &str,
) -> TaskResult<()> {
    let output = Command::new("cargo")
        .current_dir(workspace)
        .args(&args)
        .output()
        .map_err(|error| map_command_error(error, display, install_hint))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no such subcommand") || stderr.contains("no such command") {
        return Err(TaskError::ToolMissing(format!(
            "{display} is unavailable; {install_hint}"
        )));
    }

    Err(TaskError::CommandFailed {
        program: display.to_string(),
        status: output.status,
    })
}

/// Lists tracked files using `git ls-files -z`.
pub fn list_tracked_files(workspace: &std::path::Path) -> TaskResult<Vec<PathBuf>> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "git ls-files",
                "install git and ensure it is available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("git ls-files"),
            status: output.status,
        });
    }

    let mut files = Vec::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            files.push(PathBuf::from(OsString::from_vec(entry.to_vec())));
        }

        #[cfg(not(unix))]
        {
            let path = String::from_utf8(entry.to_vec()).map_err(|_| {
                TaskError::Metadata(String::from(
                    "git reported a non-UTF-8 path; binary audit requires UTF-8 file names on this platform",
                ))
            })?;
            files.push(PathBuf::from(path));
        }
    }

    Ok(files)
}

/// Returns all Rust sources tracked or untracked within the repository via git.
pub fn list_rust_sources_via_git(workspace: &std::path::Path) -> TaskResult<Vec<PathBuf>> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "git ls-files",
                "install git and ensure it is available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("git ls-files"),
            status: output.status,
        });
    }

    let mut files = Vec::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            files.push(PathBuf::from(OsString::from_vec(entry.to_vec())));
        }

        #[cfg(not(unix))]
        {
            let path = String::from_utf8(entry.to_vec()).map_err(|_| {
                TaskError::Metadata(String::from(
                    "git reported a non-UTF-8 path; placeholder scanning requires UTF-8 file names on this platform",
                ))
            })?;
            files.push(PathBuf::from(path));
        }
    }

    files.sort();
    files.dedup();
    Ok(files)
}

/// Heuristically determines whether the path references a binary file.
pub fn is_probably_binary(path: &std::path::Path) -> TaskResult<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Ok(false);
    }

    let mut file = fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let mut printable = 0usize;
    let mut control = 0usize;
    let mut inspected = 0usize;

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        inspected += read;

        for &byte in &buffer[..read] {
            match byte {
                0 => return Ok(true),
                0x07 | 0x08 | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => printable += 1,
                0x20..=0x7E => printable += 1,
                _ if byte >= 0x80 => printable += 1,
                _ => control += 1,
            }
        }

        if control > printable {
            return Ok(true);
        }

        if inspected >= buffer.len() {
            break;
        }
    }

    Ok(false)
}

/// Reads JSON metadata from `cargo metadata`.
pub fn cargo_metadata_json(workspace: &std::path::Path) -> TaskResult<JsonValue> {
    let output = Command::new("cargo")
        .current_dir(workspace)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "cargo metadata",
                "ensure cargo is installed and available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("cargo metadata"),
            status: output.status,
        });
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        TaskError::Metadata(format!("failed to parse cargo metadata JSON: {error}"))
    })
}

/// Counts the number of lines in the provided UTF-8 text file.
pub fn count_file_lines(path: &std::path::Path) -> TaskResult<usize> {
    let file = fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);
    let mut buffer = String::new();
    let mut count = 0usize;

    loop {
        buffer.clear();
        let read = reader.read_line(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += 1;
    }

    Ok(count)
}

/// Reads an environment variable that stores a positive integer, returning
/// `Ok(None)` when the variable is not set.
pub fn read_limit_env_var(name: &str) -> TaskResult<Option<usize>> {
    match env::var(name) {
        Ok(value) => {
            if value.is_empty() {
                return Err(TaskError::Validation(format!(
                    "{name} must be a positive integer, found an empty value"
                )));
            }

            let parsed = parse_positive_usize_from_env(name, &value)?;
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(TaskError::Validation(format!(
            "{name} must contain a UTF-8 encoded positive integer"
        ))),
    }
}

fn parse_positive_usize_from_env(name: &str, value: &str) -> TaskResult<usize> {
    let parsed = value.parse::<usize>().map_err(|_| {
        TaskError::Validation(format!(
            "{name} must be a positive integer, found '{value}'"
        ))
    })?;

    if parsed == 0 {
        return Err(TaskError::Validation(format!(
            "{name} must be greater than zero, found '{value}'"
        )));
    }

    Ok(parsed)
}
