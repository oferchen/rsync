use super::action::DryRunAction;

/// Formatter for dry-run output matching upstream rsync.
///
/// Provides methods to format individual dry-run actions, respecting the
/// verbosity level to control detail visibility.
#[derive(Debug, Clone, Copy)]
pub struct DryRunFormatter {
    pub(crate) verbosity: u32,
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
    #[must_use]
    pub fn format_actions(&self, actions: &[DryRunAction]) -> String {
        let mut output = String::new();
        for action in actions {
            output.push_str(&self.format_action(action));
        }
        output
    }
}
