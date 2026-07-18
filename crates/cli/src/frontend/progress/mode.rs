/// Resolved progress rendering mode used by the live renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgressMode {
    /// Per-file progress (`--progress`).
    PerFile,
    /// Overall transfer progress (`--info=progress2`).
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
    /// Resolves the setting to a concrete `ProgressMode`, or `None` when
    /// progress is disabled or unspecified.
    pub(crate) const fn resolved(self) -> Option<ProgressMode> {
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
    /// Parses a `--stderr` mode value.
    ///
    /// Mirrors upstream rsync's `options.c:1912` `OPT_STDERR` handler, which
    /// accepts any non-empty, case-sensitive prefix of `errors`, `all`, or
    /// `client` (via `strncmp(word, arg, strlen(arg))`). So `e`/`er`/`err`/
    /// `error`/`errors` all select `Errors`, but `ERRORS` and `errorsX` are
    /// rejected. Returns `None` for anything that is not such a prefix.
    pub fn from_str(s: &str) -> Option<Self> {
        if s.is_empty() {
            return None;
        }
        if "errors".starts_with(s) {
            Some(Self::Errors)
        } else if "all".starts_with(s) {
            Some(Self::All)
        } else if "client".starts_with(s) {
            Some(Self::Client)
        } else {
            None
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
        // upstream accepts every non-empty prefix of "errors".
        for value in ["e", "er", "err", "erro", "error", "errors"] {
            assert_eq!(
                StderrMode::from_str(value),
                Some(StderrMode::Errors),
                "{value}"
            );
        }
    }

    #[test]
    fn stderr_mode_from_str_all() {
        for value in ["a", "al", "all"] {
            assert_eq!(
                StderrMode::from_str(value),
                Some(StderrMode::All),
                "{value}"
            );
        }
    }

    #[test]
    fn stderr_mode_from_str_client() {
        for value in ["c", "cl", "cli", "clie", "clien", "client"] {
            assert_eq!(
                StderrMode::from_str(value),
                Some(StderrMode::Client),
                "{value}"
            );
        }
    }

    #[test]
    fn stderr_mode_from_str_is_case_sensitive() {
        // upstream uses strncmp (case-sensitive); uppercase variants are rejected.
        for value in ["ERRORS", "E", "ALL", "A", "CLIENT", "C", "Errors", "All"] {
            assert!(StderrMode::from_str(value).is_none(), "{value}");
        }
    }

    #[test]
    fn stderr_mode_from_str_invalid() {
        // Empty, non-prefixes, and over-long values (longer than the word) all fail.
        for value in ["", "invalid", "xyz", "errorsX", "allX", "ax", "x"] {
            assert!(StderrMode::from_str(value).is_none(), "{value}");
        }
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
