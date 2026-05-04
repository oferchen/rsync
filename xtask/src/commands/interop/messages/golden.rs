//! Golden message database management.
//!
//! Supports three types of message expectations:
//! 1. Exact matches - message text must match exactly
//! 2. Pattern matches - message text must match a regex pattern
//! 3. Message groups - at least N messages from a group must match

use super::matcher::{ExactMatcher, GroupMatcher, MessageMatcher, PatternMatcher};
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
    #[serde(default)]
    pub messages: Vec<StoredMessage>,
    /// Message groups where at least N must match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_groups: Vec<StoredMessageGroup>,
}

/// A stored message in the golden file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMessage {
    /// The normalized message text (for exact matching).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Regex pattern for matching (alternative to exact text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// The role trailer if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Description of the scenario that produces this message.
    pub scenario: String,
    /// Whether this message is optional (may not appear due to race conditions).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

impl StoredMessage {
    /// Create an exact match message.
    #[cfg(test)]
    pub fn exact(text: String, role: Option<String>, scenario: String) -> Self {
        Self {
            text: Some(text),
            pattern: None,
            role,
            scenario,
            optional: false,
        }
    }

    /// Create a pattern match message.
    #[cfg(test)]
    pub fn pattern(pattern: String, role: Option<String>, scenario: String) -> Self {
        Self {
            text: None,
            pattern: Some(pattern),
            role,
            scenario,
            optional: false,
        }
    }

    /// Check if this is a pattern-based message.
    #[cfg(test)]
    pub fn is_pattern(&self) -> bool {
        self.pattern.is_some()
    }

    /// Get the text for display purposes.
    #[allow(dead_code)] // May be useful for debugging/display
    pub fn display_text(&self) -> &str {
        self.text
            .as_deref()
            .or(self.pattern.as_deref())
            .unwrap_or("<empty>")
    }

    /// Convert to a MessageMatcher.
    pub fn to_matcher(&self) -> Box<dyn MessageMatcher> {
        if let Some(ref pattern) = self.pattern {
            Box::new(PatternMatcher::new(
                pattern.clone(),
                self.role.clone(),
                self.scenario.clone(),
                self.optional,
            ))
        } else {
            Box::new(ExactMatcher {
                text: self.text.clone().unwrap_or_default(),
                role: self.role.clone(),
                scenario: self.scenario.clone(),
                optional: self.optional,
            })
        }
    }
}

/// A group of messages where at least N must match.
///
/// This is useful for race-condition scenarios where different messages
/// may appear depending on timing, but at least one should always be present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMessageGroup {
    /// Name of this group for reporting.
    pub name: String,
    /// Description of the scenario that produces these messages.
    pub scenario: String,
    /// How many messages from this group must match (default: 1).
    #[serde(default = "default_require_at_least")]
    pub require_at_least: usize,
    /// The messages in this group (can be exact or pattern).
    pub messages: Vec<GroupMessage>,
}

fn default_require_at_least() -> usize {
    1
}

/// A message within a group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupMessage {
    /// Exact text to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Regex pattern to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Role requirement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl StoredMessageGroup {
    /// Convert to a GroupMatcher.
    pub fn to_matcher(&self) -> GroupMatcher {
        let matchers: Vec<Box<dyn MessageMatcher>> = self
            .messages
            .iter()
            .map(|m| -> Box<dyn MessageMatcher> {
                if let Some(ref pattern) = m.pattern {
                    Box::new(PatternMatcher::new(
                        pattern.clone(),
                        m.role.clone(),
                        self.scenario.clone(),
                        false, // Individual group members are not optional
                    ))
                } else {
                    Box::new(ExactMatcher {
                        text: m.text.clone().unwrap_or_default(),
                        role: m.role.clone(),
                        scenario: self.scenario.clone(),
                        optional: false,
                    })
                }
            })
            .collect();

        GroupMatcher::new(
            self.name.clone(),
            matchers,
            self.require_at_least,
            self.scenario.clone(),
        )
    }
}

impl GoldenMessages {
    /// Create a new golden messages database.
    pub fn new(version: String) -> Self {
        Self {
            version,
            messages: Vec::new(),
            message_groups: Vec::new(),
        }
    }

    /// Add a normalized message to the database.
    pub fn add_message(&mut self, msg: &NormalizedMessage, scenario: &str) {
        self.messages.push(StoredMessage {
            text: Some(msg.text.clone()),
            pattern: None,
            role: msg.role.clone(),
            scenario: scenario.to_owned(),
            optional: false,
        });
    }

    /// Get messages for a specific scenario.
    #[allow(dead_code)] // API for potential future use
    pub fn get_messages_for_scenario(&self, scenario: &str) -> Vec<&StoredMessage> {
        self.messages
            .iter()
            .filter(|m| m.scenario == scenario)
            .collect()
    }

    /// Get message groups for a specific scenario.
    #[allow(dead_code)] // API for potential future use
    pub fn get_groups_for_scenario(&self, scenario: &str) -> Vec<&StoredMessageGroup> {
        self.message_groups
            .iter()
            .filter(|g| g.scenario == scenario)
            .collect()
    }

    /// Get all matchers for a specific scenario.
    pub fn get_matchers_for_scenario(
        &self,
        scenario: &str,
    ) -> (Vec<Box<dyn MessageMatcher>>, Vec<GroupMatcher>) {
        let matchers: Vec<Box<dyn MessageMatcher>> = self
            .messages
            .iter()
            .filter(|m| m.scenario == scenario)
            .map(|m| m.to_matcher())
            .collect();

        let groups: Vec<GroupMatcher> = self
            .message_groups
            .iter()
            .filter(|g| g.scenario == scenario)
            .map(|g| g.to_matcher())
            .collect();

        (matchers, groups)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stored_message_exact() {
        let msg = StoredMessage::exact(
            "rsync error: test".to_owned(),
            Some("sender".to_owned()),
            "test_scenario".to_owned(),
        );

        assert!(!msg.is_pattern());
        assert_eq!(msg.display_text(), "rsync error: test");
    }

    #[test]
    fn test_stored_message_pattern() {
        let msg = StoredMessage::pattern(
            r"rsync error: .* at io\.c\(\d+\)".to_owned(),
            None,
            "test_scenario".to_owned(),
        );

        assert!(msg.is_pattern());
    }

    #[test]
    fn test_golden_messages_serialization() {
        let mut golden = GoldenMessages::new("3.4.1".to_owned());

        golden.messages.push(StoredMessage::exact(
            "test message".to_owned(),
            None,
            "test".to_owned(),
        ));

        golden.message_groups.push(StoredMessageGroup {
            name: "ipc_errors".to_owned(),
            scenario: "remote_not_found".to_owned(),
            require_at_least: 1,
            messages: vec![
                GroupMessage {
                    text: None,
                    pattern: Some(r"rsync error: .* at io\.c\(\d+\)".to_owned()),
                    role: Some("sender".to_owned()),
                },
                GroupMessage {
                    text: Some("rsync: connection unexpectedly closed".to_owned()),
                    pattern: None,
                    role: None,
                },
            ],
        });

        let toml_str = toml::to_string_pretty(&golden).unwrap();
        assert!(toml_str.contains("message_groups"));
        assert!(toml_str.contains("require_at_least"));

        // Verify it can be deserialized
        let parsed: GoldenMessages = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.version, "3.4.1");
        assert_eq!(parsed.message_groups.len(), 1);
    }
}
