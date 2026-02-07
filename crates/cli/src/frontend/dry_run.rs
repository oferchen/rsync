#![deny(unsafe_code)]

//! Implements upstream rsync's --dry-run (-n) output format.
//!
//! The dry-run feature simulates a transfer without actually performing it,
//! showing what would be transferred, deleted, or modified. This module provides
//! types and formatters for displaying planned actions in a format that matches
//! upstream rsync.
//!
//! # Examples
//!
//! ```
//! use cli::dry_run::{DryRunAction, DryRunSummary};
//!
//! let mut summary = DryRunSummary::new();
//! summary.add_action(DryRunAction::SendFile {
//!     path: "file.txt".to_string(),
//!     size: 1024,
//! });
//! summary.add_action(DryRunAction::CreateDir {
//!     path: "subdir/".to_string(),
//! });
//!
//! let output = summary.format_output(1);
//! assert!(output.contains("file.txt"));
//! assert!(output.contains("subdir/"));
//! ```

/// Represents a planned action during a dry run.
///
/// Each variant represents an operation that would be performed if not running
/// in dry-run mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryRunAction {
    /// Would send this file to the remote.
    SendFile {
        /// Path relative to destination.
        path: String,
        /// File size in bytes.
        size: u64,
    },
    /// Would receive this file from the remote.
    ReceiveFile {
        /// Path relative to destination.
        path: String,
        /// File size in bytes.
        size: u64,
    },
    /// Would delete this file.
    DeleteFile {
        /// Path relative to destination.
        path: String,
    },
    /// Would delete this directory.
    DeleteDir {
        /// Path relative to destination (ends with `/`).
        path: String,
    },
    /// Would create this directory.
    CreateDir {
        /// Path relative to destination (ends with `/`).
        path: String,
    },
    /// Would update permissions on this file/directory.
    UpdatePerms {
        /// Path relative to destination.
        path: String,
    },
    /// Would create this symlink.
    CreateSymlink {
        /// Path relative to destination.
        path: String,
        /// Symlink target.
        target: String,
    },
    /// Would create this hard link.
    CreateHardlink {
        /// Path relative to destination.
        path: String,
        /// Hard link target.
        target: String,
    },
}

impl DryRunAction {
    /// Returns the path associated with this action.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::SendFile { path, .. }
            | Self::ReceiveFile { path, .. }
            | Self::DeleteFile { path }
            | Self::DeleteDir { path }
            | Self::CreateDir { path }
            | Self::UpdatePerms { path }
            | Self::CreateSymlink { path, .. }
            | Self::CreateHardlink { path, .. } => path,
        }
    }

    /// Returns the size associated with this action, if any.
    #[must_use]
    pub fn size(&self) -> Option<u64> {
        match self {
            Self::SendFile { size, .. } | Self::ReceiveFile { size, .. } => Some(*size),
            _ => None,
        }
    }

    /// Returns `true` if this action is a deletion.
    #[must_use]
    pub const fn is_deletion(&self) -> bool {
        matches!(self, Self::DeleteFile { .. } | Self::DeleteDir { .. })
    }

    /// Returns `true` if this action is a directory operation.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        matches!(self, Self::CreateDir { .. } | Self::DeleteDir { .. })
    }
}

/// Collects and formats dry-run actions.
///
/// This structure accumulates planned actions during a dry run and provides
/// methods to format them for display to the user. The output format matches
/// upstream rsync's behavior.
///
/// # Examples
///
/// ```
/// use cli::dry_run::{DryRunAction, DryRunSummary};
///
/// let mut summary = DryRunSummary::new();
/// summary.add_action(DryRunAction::SendFile {
///     path: "file.txt".to_string(),
///     size: 1024,
/// });
///
/// let output = summary.format_output(1);
/// assert!(output.contains("file.txt"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunSummary {
    actions: Vec<DryRunAction>,
    total_size: u64,
}

impl DryRunSummary {
    /// Creates a new empty dry-run summary.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            actions: Vec::new(),
            total_size: 0,
        }
    }

    /// Adds an action to this summary.
    ///
    /// If the action has an associated size, it is added to the total size.
    pub fn add_action(&mut self, action: DryRunAction) {
        if let Some(size) = action.size() {
            self.total_size = self.total_size.saturating_add(size);
        }
        self.actions.push(action);
    }

    /// Returns the number of actions in this summary.
    #[must_use]
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    /// Returns the total size of all file actions in bytes.
    #[must_use]
    pub const fn total_size(&self) -> u64 {
        self.total_size
    }

    /// Returns a slice of all actions.
    #[must_use]
    pub fn actions(&self) -> &[DryRunAction] {
        &self.actions
    }

    /// Formats all actions for display.
    ///
    /// The output format depends on the verbosity level:
    /// - `0`: No output (silent)
    /// - `1`: Show file names (default)
    /// - `2+`: Show file names with additional details
    ///
    /// # Examples
    ///
    /// ```
    /// use cli::dry_run::{DryRunAction, DryRunSummary};
    ///
    /// let mut summary = DryRunSummary::new();
    /// summary.add_action(DryRunAction::SendFile {
    ///     path: "file.txt".to_string(),
    ///     size: 1024,
    /// });
    ///
    /// let output = summary.format_output(1);
    /// assert!(output.contains("file.txt"));
    /// ```
    #[must_use]
    pub fn format_output(&self, verbosity: u32) -> String {
        if verbosity == 0 {
            return String::new();
        }

        let mut output = String::new();

        for action in &self.actions {
            match action {
                DryRunAction::SendFile { path, .. } => {
                    output.push_str(path);
                    output.push('\n');
                }
                DryRunAction::ReceiveFile { path, .. } => {
                    output.push_str(path);
                    output.push('\n');
                }
                DryRunAction::DeleteFile { path } => {
                    if verbosity >= 1 {
                        output.push_str("deleting ");
                        output.push_str(path);
                        output.push('\n');
                    }
                }
                DryRunAction::DeleteDir { path } => {
                    if verbosity >= 1 {
                        output.push_str("deleting ");
                        output.push_str(path);
                        output.push('\n');
                    }
                }
                DryRunAction::CreateDir { path } => {
                    output.push_str(path);
                    output.push('\n');
                }
                DryRunAction::UpdatePerms { path } => {
                    if verbosity >= 2 {
                        output.push_str(path);
                        output.push('\n');
                    }
                }
                DryRunAction::CreateSymlink { path, target } => {
                    output.push_str(path);
                    if verbosity >= 2 {
                        output.push_str(" -> ");
                        output.push_str(target);
                    }
                    output.push('\n');
                }
                DryRunAction::CreateHardlink { path, target } => {
                    output.push_str(path);
                    if verbosity >= 2 {
                        output.push_str(" => ");
                        output.push_str(target);
                    }
                    output.push('\n');
                }
            }
        }

        output
    }

    /// Formats the summary line that appears at the end of a dry run.
    ///
    /// This produces output like:
    /// ```text
    /// sent 0 bytes  received 1,234 bytes  0.00 bytes/sec
    /// total size is 1,234  speedup is 0.00 (DRY RUN)
    /// ```
    ///
    /// # Examples
    ///
    /// ```
    /// use cli::dry_run::DryRunSummary;
    ///
    /// let summary = DryRunSummary::new();
    /// let output = summary.format_summary();
    /// assert!(output.contains("(DRY RUN)"));
    /// ```
    #[must_use]
    pub fn format_summary(&self) -> String {
        // In a real dry run, no bytes are actually sent or received
        format!(
            "sent 0 bytes  received 0 bytes  0.00 bytes/sec\n\
             total size is {}  speedup is 0.00 (DRY RUN)\n",
            format_number_with_commas(self.total_size)
        )
    }
}

impl Default for DryRunSummary {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for dry-run output matching upstream rsync.
///
/// This structure provides methods to format dry-run actions in a way that
/// matches the output format of upstream rsync.
///
/// # Examples
///
/// ```
/// use cli::dry_run::{DryRunAction, DryRunFormatter};
///
/// let formatter = DryRunFormatter::new(1);
/// let action = DryRunAction::SendFile {
///     path: "file.txt".to_string(),
///     size: 1024,
/// };
///
/// let output = formatter.format_action(&action);
/// assert_eq!(output, "file.txt\n");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct DryRunFormatter {
    verbosity: u32,
}

impl DryRunFormatter {
    /// Creates a new formatter with the specified verbosity level.
    #[must_use]
    pub const fn new(verbosity: u32) -> Self {
        Self { verbosity }
    }

    /// Formats a single action for display.
    ///
    /// Returns an empty string if the action should not be displayed at the
    /// current verbosity level.
    ///
    /// # Examples
    ///
    /// ```
    /// use cli::dry_run::{DryRunAction, DryRunFormatter};
    ///
    /// let formatter = DryRunFormatter::new(1);
    /// let action = DryRunAction::DeleteFile {
    ///     path: "old.txt".to_string(),
    /// };
    ///
    /// let output = formatter.format_action(&action);
    /// assert!(output.contains("deleting old.txt"));
    /// ```
    #[must_use]
    pub fn format_action(&self, action: &DryRunAction) -> String {
        if self.verbosity == 0 {
            return String::new();
        }

        match action {
            DryRunAction::SendFile { path, .. } | DryRunAction::ReceiveFile { path, .. } => {
                format!("{path}\n")
            }
            DryRunAction::DeleteFile { path } | DryRunAction::DeleteDir { path } => {
                format!("deleting {path}\n")
            }
            DryRunAction::CreateDir { path } => {
                format!("{path}\n")
            }
            DryRunAction::UpdatePerms { path } => {
                if self.verbosity >= 2 {
                    format!("{path}\n")
                } else {
                    String::new()
                }
            }
            DryRunAction::CreateSymlink { path, target } => {
                if self.verbosity >= 2 {
                    format!("{path} -> {target}\n")
                } else {
                    format!("{path}\n")
                }
            }
            DryRunAction::CreateHardlink { path, target } => {
                if self.verbosity >= 2 {
                    format!("{path} => {target}\n")
                } else {
                    format!("{path}\n")
                }
            }
        }
    }

    /// Formats a list of actions for display.
    ///
    /// # Examples
    ///
    /// ```
    /// use cli::dry_run::{DryRunAction, DryRunFormatter};
    ///
    /// let formatter = DryRunFormatter::new(1);
    /// let actions = vec![
    ///     DryRunAction::SendFile {
    ///         path: "file1.txt".to_string(),
    ///         size: 100,
    ///     },
    ///     DryRunAction::SendFile {
    ///         path: "file2.txt".to_string(),
    ///         size: 200,
    ///     },
    /// ];
    ///
    /// let output = formatter.format_actions(&actions);
    /// assert!(output.contains("file1.txt"));
    /// assert!(output.contains("file2.txt"));
    /// ```
    #[must_use]
    pub fn format_actions(&self, actions: &[DryRunAction]) -> String {
        let mut output = String::new();
        for action in actions {
            output.push_str(&self.format_action(action));
        }
        output
    }
}

/// Formats a number with thousands separators (commas).
///
/// This matches the formatting used by upstream rsync for file sizes and counts.
///
/// # Examples
///
/// ```
/// use cli::dry_run::format_number_with_commas;
///
/// assert_eq!(format_number_with_commas(0), "0");
/// assert_eq!(format_number_with_commas(123), "123");
/// assert_eq!(format_number_with_commas(1234), "1,234");
/// assert_eq!(format_number_with_commas(1234567), "1,234,567");
/// ```
#[must_use]
pub fn format_number_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();

    if len <= 3 {
        return s;
    }

    let mut result = String::with_capacity(len + (len - 1) / 3);
    let first_group_len = len % 3;

    if first_group_len > 0 {
        result.push_str(&s[..first_group_len]);
        if len > first_group_len {
            result.push(',');
        }
    }

    let mut i = first_group_len;
    while i < len {
        if i > first_group_len && i > 0 {
            result.push(',');
        }
        result.push_str(&s[i..i + 3]);
        i += 3;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- DryRunAction path extraction ----

    #[test]
    fn action_path_returns_send_file_path() {
        let action = DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 100,
        };
        assert_eq!(action.path(), "file.txt");
    }

    #[test]
    fn action_path_returns_receive_file_path() {
        let action = DryRunAction::ReceiveFile {
            path: "file.txt".to_string(),
            size: 100,
        };
        assert_eq!(action.path(), "file.txt");
    }

    #[test]
    fn action_path_returns_delete_file_path() {
        let action = DryRunAction::DeleteFile {
            path: "old.txt".to_string(),
        };
        assert_eq!(action.path(), "old.txt");
    }

    #[test]
    fn action_path_returns_create_dir_path() {
        let action = DryRunAction::CreateDir {
            path: "subdir/".to_string(),
        };
        assert_eq!(action.path(), "subdir/");
    }

    // ---- DryRunAction size extraction ----

    #[test]
    fn action_size_returns_send_file_size() {
        let action = DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 1024,
        };
        assert_eq!(action.size(), Some(1024));
    }

    #[test]
    fn action_size_returns_receive_file_size() {
        let action = DryRunAction::ReceiveFile {
            path: "file.txt".to_string(),
            size: 2048,
        };
        assert_eq!(action.size(), Some(2048));
    }

    #[test]
    fn action_size_returns_none_for_delete() {
        let action = DryRunAction::DeleteFile {
            path: "old.txt".to_string(),
        };
        assert_eq!(action.size(), None);
    }

    #[test]
    fn action_size_returns_none_for_create_dir() {
        let action = DryRunAction::CreateDir {
            path: "subdir/".to_string(),
        };
        assert_eq!(action.size(), None);
    }

    // ---- DryRunAction type detection ----

    #[test]
    fn action_is_deletion_returns_true_for_delete_file() {
        let action = DryRunAction::DeleteFile {
            path: "old.txt".to_string(),
        };
        assert!(action.is_deletion());
    }

    #[test]
    fn action_is_deletion_returns_true_for_delete_dir() {
        let action = DryRunAction::DeleteDir {
            path: "olddir/".to_string(),
        };
        assert!(action.is_deletion());
    }

    #[test]
    fn action_is_deletion_returns_false_for_send_file() {
        let action = DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 100,
        };
        assert!(!action.is_deletion());
    }

    #[test]
    fn action_is_directory_returns_true_for_create_dir() {
        let action = DryRunAction::CreateDir {
            path: "subdir/".to_string(),
        };
        assert!(action.is_directory());
    }

    #[test]
    fn action_is_directory_returns_true_for_delete_dir() {
        let action = DryRunAction::DeleteDir {
            path: "olddir/".to_string(),
        };
        assert!(action.is_directory());
    }

    #[test]
    fn action_is_directory_returns_false_for_send_file() {
        let action = DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 100,
        };
        assert!(!action.is_directory());
    }

    // ---- DryRunSummary basic operations ----

    #[test]
    fn summary_new_creates_empty_summary() {
        let summary = DryRunSummary::new();
        assert_eq!(summary.action_count(), 0);
        assert_eq!(summary.total_size(), 0);
    }

    #[test]
    fn summary_default_is_same_as_new() {
        assert_eq!(DryRunSummary::default(), DryRunSummary::new());
    }

    #[test]
    fn summary_add_action_increments_count() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 100,
        });
        assert_eq!(summary.action_count(), 1);
    }

    #[test]
    fn summary_add_action_updates_total_size() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "file1.txt".to_string(),
            size: 100,
        });
        summary.add_action(DryRunAction::ReceiveFile {
            path: "file2.txt".to_string(),
            size: 200,
        });
        assert_eq!(summary.total_size(), 300);
    }

    #[test]
    fn summary_add_action_ignores_size_for_delete() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::DeleteFile {
            path: "old.txt".to_string(),
        });
        assert_eq!(summary.total_size(), 0);
    }

    #[test]
    fn summary_actions_returns_all_actions() {
        let mut summary = DryRunSummary::new();
        let action1 = DryRunAction::SendFile {
            path: "file1.txt".to_string(),
            size: 100,
        };
        let action2 = DryRunAction::DeleteFile {
            path: "old.txt".to_string(),
        };
        summary.add_action(action1.clone());
        summary.add_action(action2.clone());
        assert_eq!(summary.actions(), &[action1, action2]);
    }

    // ---- DryRunSummary output formatting ----

    #[test]
    fn summary_format_output_verbosity_zero_returns_empty() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 100,
        });
        assert_eq!(summary.format_output(0), "");
    }

    #[test]
    fn summary_format_output_shows_send_file() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 100,
        });
        let output = summary.format_output(1);
        assert!(output.contains("file.txt"));
    }

    #[test]
    fn summary_format_output_shows_receive_file() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::ReceiveFile {
            path: "file.txt".to_string(),
            size: 100,
        });
        let output = summary.format_output(1);
        assert!(output.contains("file.txt"));
    }

    #[test]
    fn summary_format_output_shows_delete_with_prefix() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::DeleteFile {
            path: "old.txt".to_string(),
        });
        let output = summary.format_output(1);
        assert!(output.contains("deleting old.txt"));
    }

    #[test]
    fn summary_format_output_shows_create_dir() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::CreateDir {
            path: "subdir/".to_string(),
        });
        let output = summary.format_output(1);
        assert!(output.contains("subdir/"));
    }

    #[test]
    fn summary_format_output_shows_symlink_at_verbosity_two() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::CreateSymlink {
            path: "link".to_string(),
            target: "target".to_string(),
        });
        let output = summary.format_output(2);
        assert!(output.contains("link -> target"));
    }

    #[test]
    fn summary_format_output_shows_symlink_without_target_at_verbosity_one() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::CreateSymlink {
            path: "link".to_string(),
            target: "target".to_string(),
        });
        let output = summary.format_output(1);
        assert!(output.contains("link\n"));
        assert!(!output.contains("->"));
    }

    #[test]
    fn summary_format_output_shows_hardlink_at_verbosity_two() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::CreateHardlink {
            path: "link".to_string(),
            target: "target".to_string(),
        });
        let output = summary.format_output(2);
        assert!(output.contains("link => target"));
    }

    #[test]
    fn summary_format_output_hides_perms_at_verbosity_one() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::UpdatePerms {
            path: "file.txt".to_string(),
        });
        let output = summary.format_output(1);
        assert_eq!(output, "");
    }

    #[test]
    fn summary_format_output_shows_perms_at_verbosity_two() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::UpdatePerms {
            path: "file.txt".to_string(),
        });
        let output = summary.format_output(2);
        assert!(output.contains("file.txt"));
    }

    #[test]
    fn summary_format_output_handles_multiple_actions() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "file1.txt".to_string(),
            size: 100,
        });
        summary.add_action(DryRunAction::SendFile {
            path: "file2.txt".to_string(),
            size: 200,
        });
        summary.add_action(DryRunAction::DeleteFile {
            path: "old.txt".to_string(),
        });
        let output = summary.format_output(1);
        assert!(output.contains("file1.txt"));
        assert!(output.contains("file2.txt"));
        assert!(output.contains("deleting old.txt"));
    }

    // ---- DryRunSummary summary line ----

    #[test]
    fn summary_format_summary_includes_dry_run_marker() {
        let summary = DryRunSummary::new();
        let output = summary.format_summary();
        assert!(output.contains("(DRY RUN)"));
    }

    #[test]
    fn summary_format_summary_shows_zero_bytes_sent_received() {
        let summary = DryRunSummary::new();
        let output = summary.format_summary();
        assert!(output.contains("sent 0 bytes  received 0 bytes"));
    }

    #[test]
    fn summary_format_summary_shows_total_size() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 1234,
        });
        let output = summary.format_summary();
        assert!(output.contains("total size is 1,234"));
    }

    #[test]
    fn summary_format_summary_shows_speedup() {
        let summary = DryRunSummary::new();
        let output = summary.format_summary();
        assert!(output.contains("speedup is 0.00"));
    }

    // ---- DryRunFormatter ----

    #[test]
    fn formatter_new_creates_formatter() {
        let formatter = DryRunFormatter::new(1);
        assert_eq!(formatter.verbosity, 1);
    }

    #[test]
    fn formatter_format_action_returns_empty_at_verbosity_zero() {
        let formatter = DryRunFormatter::new(0);
        let action = DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 100,
        };
        assert_eq!(formatter.format_action(&action), "");
    }

    #[test]
    fn formatter_format_action_formats_send_file() {
        let formatter = DryRunFormatter::new(1);
        let action = DryRunAction::SendFile {
            path: "file.txt".to_string(),
            size: 100,
        };
        assert_eq!(formatter.format_action(&action), "file.txt\n");
    }

    #[test]
    fn formatter_format_action_formats_delete_file() {
        let formatter = DryRunFormatter::new(1);
        let action = DryRunAction::DeleteFile {
            path: "old.txt".to_string(),
        };
        assert_eq!(formatter.format_action(&action), "deleting old.txt\n");
    }

    #[test]
    fn formatter_format_action_formats_create_dir() {
        let formatter = DryRunFormatter::new(1);
        let action = DryRunAction::CreateDir {
            path: "subdir/".to_string(),
        };
        assert_eq!(formatter.format_action(&action), "subdir/\n");
    }

    #[test]
    fn formatter_format_action_formats_symlink_with_target_at_verbosity_two() {
        let formatter = DryRunFormatter::new(2);
        let action = DryRunAction::CreateSymlink {
            path: "link".to_string(),
            target: "target".to_string(),
        };
        assert_eq!(formatter.format_action(&action), "link -> target\n");
    }

    #[test]
    fn formatter_format_action_formats_symlink_without_target_at_verbosity_one() {
        let formatter = DryRunFormatter::new(1);
        let action = DryRunAction::CreateSymlink {
            path: "link".to_string(),
            target: "target".to_string(),
        };
        assert_eq!(formatter.format_action(&action), "link\n");
    }

    #[test]
    fn formatter_format_actions_formats_multiple_actions() {
        let formatter = DryRunFormatter::new(1);
        let actions = vec![
            DryRunAction::SendFile {
                path: "file1.txt".to_string(),
                size: 100,
            },
            DryRunAction::SendFile {
                path: "file2.txt".to_string(),
                size: 200,
            },
        ];
        let output = formatter.format_actions(&actions);
        assert_eq!(output, "file1.txt\nfile2.txt\n");
    }

    // ---- Number formatting ----

    #[test]
    fn format_number_with_commas_handles_zero() {
        assert_eq!(format_number_with_commas(0), "0");
    }

    #[test]
    fn format_number_with_commas_handles_small_numbers() {
        assert_eq!(format_number_with_commas(1), "1");
        assert_eq!(format_number_with_commas(12), "12");
        assert_eq!(format_number_with_commas(123), "123");
    }

    #[test]
    fn format_number_with_commas_adds_single_comma() {
        assert_eq!(format_number_with_commas(1234), "1,234");
        assert_eq!(format_number_with_commas(9999), "9,999");
    }

    #[test]
    fn format_number_with_commas_adds_multiple_commas() {
        assert_eq!(format_number_with_commas(1234567), "1,234,567");
        assert_eq!(format_number_with_commas(1234567890), "1,234,567,890");
    }

    #[test]
    fn format_number_with_commas_handles_large_numbers() {
        assert_eq!(
            format_number_with_commas(9_999_999_999_999_999_999),
            "9,999,999,999,999,999,999"
        );
    }

    // ---- Path with special characters ----

    #[test]
    fn summary_format_output_handles_paths_with_spaces() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "file with spaces.txt".to_string(),
            size: 100,
        });
        let output = summary.format_output(1);
        assert!(output.contains("file with spaces.txt"));
    }

    #[test]
    fn summary_format_output_handles_paths_with_unicode() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "文件.txt".to_string(),
            size: 100,
        });
        let output = summary.format_output(1);
        assert!(output.contains("文件.txt"));
    }

    // ---- Large file sizes ----

    #[test]
    fn summary_handles_large_file_sizes() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "large.bin".to_string(),
            size: 10_000_000_000, // 10 GB
        });
        assert_eq!(summary.total_size(), 10_000_000_000);
        let formatted = summary.format_summary();
        assert!(formatted.contains("10,000,000,000"));
    }

    #[test]
    fn summary_handles_size_overflow_gracefully() {
        let mut summary = DryRunSummary::new();
        summary.add_action(DryRunAction::SendFile {
            path: "file1.bin".to_string(),
            size: u64::MAX,
        });
        summary.add_action(DryRunAction::SendFile {
            path: "file2.bin".to_string(),
            size: 1,
        });
        // Should saturate at u64::MAX, not wrap around
        assert_eq!(summary.total_size(), u64::MAX);
    }

    // ---- Empty action list ----

    #[test]
    fn summary_format_output_empty_list_returns_empty_string() {
        let summary = DryRunSummary::new();
        let output = summary.format_output(1);
        assert_eq!(output, "");
    }

    #[test]
    fn summary_format_summary_empty_list_shows_zero_size() {
        let summary = DryRunSummary::new();
        let output = summary.format_summary();
        assert!(output.contains("total size is 0"));
    }
}
