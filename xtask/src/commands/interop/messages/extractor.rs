//! Message extraction and parsing from rsync output.

use crate::error::{TaskError, TaskResult};
use std::path::Path;
use std::process::{Command, Stdio};

/// A message extracted from rsync output.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message {
    /// The raw message text (before normalization).
    pub text: String,
    /// The severity level (Error, Warning, Info) if detected.
    pub severity: Option<Severity>,
    /// The role trailer (e.g., "sender", "receiver") if present.
    pub role: Option<String>,
}

/// Message severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
    #[allow(dead_code)]
    Info,
}

impl Message {
    /// Create a new message from raw text.
    pub fn new(text: String) -> Self {
        let (severity, role) = Self::parse_metadata(&text);
        Self {
            text,
            severity,
            role,
        }
    }

    /// Parse severity and role trailer from message text.
    fn parse_metadata(text: &str) -> (Option<Severity>, Option<String>) {
        let severity = if text.contains("error") || text.contains("ERROR") {
            Some(Severity::Error)
        } else if text.contains("warning") || text.contains("WARNING") {
            Some(Severity::Warning)
        } else {
            None
        };

        // Extract role trailer like [sender], [receiver], [generator]
        let role = if let Some(start) = text.rfind('[') {
            if let Some(end) = text[start..].find(']') {
                let trailer = &text[start + 1..start + end];
                // Check if it's a role (not a code or other bracket content)
                if trailer.contains("sender")
                    || trailer.contains("receiver")
                    || trailer.contains("generator")
                    || trailer.contains("server")
                    || trailer.contains("client")
                    || trailer.contains("daemon")
                {
                    // Extract just the role part (before any =)
                    let role_part = trailer.split('=').next().unwrap_or(trailer);
                    Some(role_part.trim().to_owned())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        (severity, role)
    }
}

/// Extract messages from rsync stderr output.
pub fn extract_messages_from_output(stderr: &str) -> Vec<Message> {
    stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Message::new(line.to_owned()))
        .collect()
}

/// Run rsync and capture stderr messages.
#[allow(dead_code)]
pub fn run_and_extract_messages(
    rsync_binary: &Path,
    args: &[String],
    work_dir: &Path,
) -> TaskResult<Vec<Message>> {
    let mut cmd = Command::new(rsync_binary);
    cmd.args(args)
        .current_dir(work_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let output = cmd.output().map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to execute rsync: {}", e),
        ))
    })?;

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(extract_messages_from_output(&stderr))
}

/// Message test scenario for capturing specific messages.
#[derive(Debug, Clone)]
pub struct MessageScenario {
    /// Unique name for this scenario.
    pub name: String,
    /// Command-line arguments to pass to rsync.
    pub args: Vec<String>,
    /// Optional shell commands to run before executing the test.
    pub setup: Option<String>,
    /// Description of what messages this scenario should produce.
    #[allow(dead_code)]
    pub description: String,
}

/// Options for running message extraction.
#[derive(Debug, Clone, Default)]
pub struct ExtractorOptions {
    /// Enable verbose output.
    pub verbose: bool,
    /// Show stdout/stderr from rsync commands.
    pub show_output: bool,
    /// Directory to save rsync logs (uses --log-file).
    pub log_dir: Option<String>,
    /// Version string for log file naming.
    pub version: Option<String>,
}

impl MessageScenario {
    /// Execute this scenario and extract messages.
    pub fn execute(
        &self,
        rsync_binary: &Path,
        options: &ExtractorOptions,
    ) -> TaskResult<Vec<Message>> {
        let temp_dir = tempfile::tempdir().map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to create temp dir for scenario '{}': {}",
                    self.name, e
                ),
            ))
        })?;

        let work_dir = temp_dir.path();

        if options.verbose {
            eprintln!(
                "[extractor] Executing scenario '{}' in {}",
                self.name,
                work_dir.display()
            );
        }

        // Run setup commands if specified
        if let Some(ref setup) = self.setup {
            if options.verbose {
                eprintln!("[extractor] Running setup: {}", setup);
            }

            Command::new("bash")
                .arg("-c")
                .arg(setup)
                .current_dir(work_dir)
                .status()
                .map_err(|e| {
                    TaskError::Io(std::io::Error::new(
                        e.kind(),
                        format!("Failed to run setup for '{}': {}", self.name, e),
                    ))
                })?;
        }

        // Replace "rsync" in args with the actual binary path
        let mut cmd_args = self.args.clone();
        if !cmd_args.is_empty() && cmd_args[0] == "rsync" {
            cmd_args[0] = rsync_binary.to_string_lossy().to_string();
        }

        // Add --log-file if log_dir is specified
        if let Some(ref log_dir) = options.log_dir {
            let version_str = options.version.as_deref().unwrap_or("unknown");
            let log_file = format!("{}/{}-{}-msg.log", log_dir, self.name, version_str);
            cmd_args.push(format!("--log-file={}", log_file));
        }

        if options.verbose {
            eprintln!("[extractor] Executing: {:?}", cmd_args);
        }

        // Execute and capture output
        let mut cmd = Command::new(rsync_binary);
        cmd.args(&cmd_args[1..])
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd.output().map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to execute rsync: {}", e),
            ))
        })?;

        // Display output if requested
        if options.show_output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stdout.is_empty() {
                eprintln!("[extractor] stdout:\n{}", stdout);
            }
            if !stderr.is_empty() {
                eprintln!("[extractor] stderr:\n{}", stderr);
            }
        }

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(extract_messages_from_output(&stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_error_message_with_role() {
        let msg = Message::new("rsync: error in file IO [sender=0.5.0]".to_owned());
        assert_eq!(msg.severity, Some(Severity::Error));
        assert_eq!(msg.role, Some("sender".to_owned()));
    }

    #[test]
    fn test_parse_warning_message() {
        let msg = Message::new("rsync: warning: some files vanished".to_owned());
        assert_eq!(msg.severity, Some(Severity::Warning));
    }

    #[test]
    fn test_extract_messages_from_output() {
        let stderr = "rsync: error in file IO\nrsync: warning: file vanished\n\n";
        let messages = extract_messages_from_output(stderr);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].severity, Some(Severity::Error));
        assert_eq!(messages[1].severity, Some(Severity::Warning));
    }
}
