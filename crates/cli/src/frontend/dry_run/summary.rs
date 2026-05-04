use super::action::DryRunAction;
use super::format::format_number_with_commas;

/// Collects and formats dry-run actions.
///
/// Accumulates planned actions during a dry run and provides methods to format
/// them for display to the user. The output format matches upstream rsync's
/// `--dry-run` (-n) behavior.
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
    /// Produces output matching upstream rsync's dry-run footer:
    /// ```text
    /// sent 0 bytes  received 1,234 bytes  0.00 bytes/sec
    /// total size is 1,234  speedup is 0.00 (DRY RUN)
    /// ```
    #[must_use]
    pub fn format_summary(&self) -> String {
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
