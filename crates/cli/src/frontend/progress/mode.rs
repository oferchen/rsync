#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgressMode {
    PerFile,
    Overall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgressSetting {
    Unspecified,
    Disabled,
    PerFile,
    Overall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NameOutputLevel {
    Disabled,
    UpdatedOnly,
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
