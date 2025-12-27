#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgressMode {
    PerFile,
    Overall,
}

/// Progress reporting mode selected via `--progress` or `--no-progress`.
///
/// **Warning**: This type is exposed via `cli::test_utils` for integration
/// tests only. It is not part of the stable public API.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProgressSetting {
    /// No explicit progress flag was provided.
    #[default]
    Unspecified,
    /// Progress reporting explicitly disabled via `--no-progress`.
    Disabled,
    /// Per-file progress reporting enabled.
    PerFile,
    /// Overall transfer progress reporting enabled.
    Overall,
}

/// Verbosity level for file name output during transfers.
///
/// **Warning**: This type is exposed via `cli::test_utils` for integration
/// tests only. It is not part of the stable public API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameOutputLevel {
    /// No file names are printed (quiet mode).
    Disabled,
    /// Print only updated file names.
    UpdatedOnly,
    /// Print both updated and unchanged file names.
    UpdatedAndUnchanged,
}

impl ProgressSetting {
    pub(crate) fn resolved(self) -> Option<ProgressMode> {
        match self {
            Self::PerFile => Some(ProgressMode::PerFile),
            Self::Overall => Some(ProgressMode::Overall),
            Self::Disabled | Self::Unspecified => None,
        }
    }
}

/// Stderr output mode controlling how messages are routed.
///
/// This corresponds to the `--stderr=MODE` option in upstream rsync.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StderrMode {
    /// Send only errors to stderr (default).
    #[default]
    Errors,
    /// Send all messages to stderr.
    All,
    /// Client errors to stderr, server errors mixed with data.
    Client,
}

impl StderrMode {
    /// Parses a stderr mode string value.
    ///
    /// Returns `None` for unrecognized values.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "errors" | "e" => Some(Self::Errors),
            "all" | "a" => Some(Self::All),
            "client" | "c" => Some(Self::Client),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_mode_eq() {
        assert_eq!(ProgressMode::PerFile, ProgressMode::PerFile);
        assert_eq!(ProgressMode::Overall, ProgressMode::Overall);
        assert_ne!(ProgressMode::PerFile, ProgressMode::Overall);
    }

    #[test]
    fn progress_setting_default() {
        let setting = ProgressSetting::default();
        assert_eq!(setting, ProgressSetting::Unspecified);
    }

    #[test]
    fn progress_setting_resolved_per_file() {
        assert_eq!(
            ProgressSetting::PerFile.resolved(),
            Some(ProgressMode::PerFile)
        );
    }

    #[test]
    fn progress_setting_resolved_overall() {
        assert_eq!(
            ProgressSetting::Overall.resolved(),
            Some(ProgressMode::Overall)
        );
    }

    #[test]
    fn progress_setting_resolved_disabled() {
        assert_eq!(ProgressSetting::Disabled.resolved(), None);
    }

    #[test]
    fn progress_setting_resolved_unspecified() {
        assert_eq!(ProgressSetting::Unspecified.resolved(), None);
    }

    #[test]
    fn stderr_mode_default() {
        assert_eq!(StderrMode::default(), StderrMode::Errors);
    }

    #[test]
    fn stderr_mode_from_str_errors() {
        assert_eq!(StderrMode::from_str("errors"), Some(StderrMode::Errors));
        assert_eq!(StderrMode::from_str("e"), Some(StderrMode::Errors));
        assert_eq!(StderrMode::from_str("ERRORS"), Some(StderrMode::Errors));
        assert_eq!(StderrMode::from_str("E"), Some(StderrMode::Errors));
    }

    #[test]
    fn stderr_mode_from_str_all() {
        assert_eq!(StderrMode::from_str("all"), Some(StderrMode::All));
        assert_eq!(StderrMode::from_str("a"), Some(StderrMode::All));
        assert_eq!(StderrMode::from_str("ALL"), Some(StderrMode::All));
        assert_eq!(StderrMode::from_str("A"), Some(StderrMode::All));
    }

    #[test]
    fn stderr_mode_from_str_client() {
        assert_eq!(StderrMode::from_str("client"), Some(StderrMode::Client));
        assert_eq!(StderrMode::from_str("c"), Some(StderrMode::Client));
        assert_eq!(StderrMode::from_str("CLIENT"), Some(StderrMode::Client));
        assert_eq!(StderrMode::from_str("C"), Some(StderrMode::Client));
    }

    #[test]
    fn stderr_mode_from_str_invalid() {
        assert!(StderrMode::from_str("invalid").is_none());
        assert!(StderrMode::from_str("").is_none());
        assert!(StderrMode::from_str("xyz").is_none());
    }

    #[test]
    fn name_output_level_eq() {
        assert_eq!(NameOutputLevel::Disabled, NameOutputLevel::Disabled);
        assert_eq!(NameOutputLevel::UpdatedOnly, NameOutputLevel::UpdatedOnly);
        assert_eq!(
            NameOutputLevel::UpdatedAndUnchanged,
            NameOutputLevel::UpdatedAndUnchanged
        );
        assert_ne!(NameOutputLevel::Disabled, NameOutputLevel::UpdatedOnly);
    }
}
