//! Golden message database management.

use super::normalizer::NormalizedMessage;
use crate::error::{TaskError, TaskResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Golden message database for a specific upstream version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenMessages {
    /// Version of upstream rsync this golden file represents.
    pub version: String,
    /// Normalized messages expected from this version.
    pub messages: Vec<StoredMessage>,
}

/// A stored message in the golden file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMessage {
    /// The normalized message text.
    pub text: String,
    /// The role trailer if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Description of the scenario that produces this message.
    pub scenario: String,
    /// Whether this message is optional (may not appear due to race conditions).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

impl GoldenMessages {
    /// Create a new golden messages database.
    pub const fn new(version: String) -> Self {
        Self {
            version,
            messages: Vec::new(),
        }
    }

    /// Add a normalized message to the database.
    pub fn add_message(&mut self, msg: &NormalizedMessage, scenario: &str) {
        self.messages.push(StoredMessage {
            text: msg.text.clone(),
            role: msg.role.clone(),
            scenario: scenario.to_owned(),
            optional: false,
        });
    }

    /// Get messages for a specific scenario.
    pub fn get_messages_for_scenario(&self, scenario: &str) -> Vec<&StoredMessage> {
        self.messages
            .iter()
            .filter(|m| m.scenario == scenario)
            .collect()
    }
}

/// Get the path to a golden messages file for a specific upstream version.
pub fn golden_file_path(workspace: &Path, version: &str) -> PathBuf {
    workspace.join(format!("tests/interop/messages/golden-{}.toml", version))
}

/// Load golden messages from a file.
pub fn load_golden(workspace: &Path, version: &str) -> TaskResult<GoldenMessages> {
    let path = golden_file_path(workspace, version);

    if !path.exists() {
        return Err(TaskError::Metadata(format!(
            "Golden messages file not found for version {}: {}\nRun with --regenerate to create it.",
            version,
            path.display()
        )));
    }

    let content = std::fs::read_to_string(&path).map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to read golden messages file {}: {}",
                path.display(),
                e
            ),
        ))
    })?;

    let golden: GoldenMessages = toml::from_str(&content).map_err(|e| {
        TaskError::Metadata(format!(
            "Failed to parse golden messages file {}: {}",
            path.display(),
            e
        ))
    })?;

    Ok(golden)
}

/// Save golden messages to a file.
pub fn save_golden(workspace: &Path, golden: &GoldenMessages) -> TaskResult<()> {
    let path = golden_file_path(workspace, &golden.version);

    // Ensure the directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = toml::to_string_pretty(golden)
        .map_err(|e| TaskError::Metadata(format!("Failed to serialize golden messages: {}", e)))?;

    std::fs::write(&path, content).map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to write golden messages file {}: {}",
                path.display(),
                e
            ),
        ))
    })?;

    eprintln!("[golden] Wrote golden messages file: {}", path.display());
    Ok(())
}
