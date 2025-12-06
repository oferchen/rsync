#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgressMode {
    PerFile,
    Overall,
}

/// Progress reporting mode selected via `--progress` or `--no-progress`.
///
/// **Warning**: This type is exposed via `cli::test_utils` for integration
/// tests only. It is not part of the stable public API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressSetting {
    /// No explicit progress flag was provided.
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
