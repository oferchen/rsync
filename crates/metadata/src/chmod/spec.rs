#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetSelector {
    All,
    Files,
    Directories,
}

impl TargetSelector {
    #[cfg(unix)]
    pub(crate) fn matches(self, file_type: std::fs::FileType) -> bool {
        match self {
            Self::All => true,
            Self::Files => !file_type.is_dir(),
            Self::Directories => file_type.is_dir(),
        }
    }

    #[cfg(not(unix))]
    #[allow(dead_code)]
    pub(crate) fn matches(self, _file_type: std::fs::FileType) -> bool {
        matches!(self, TargetSelector::All | TargetSelector::Files)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Operation {
    Add,
    Remove,
    Assign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WhoMask {
    pub(crate) user: bool,
    pub(crate) group: bool,
    pub(crate) other: bool,
}

impl WhoMask {
    pub(crate) const fn new(user: bool, group: bool, other: bool) -> Self {
        Self { user, group, other }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn includes_user(self) -> bool {
        self.user
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn includes_group(self) -> bool {
        self.group
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn includes_other(self) -> bool {
        self.other
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn covers_all(self) -> bool {
        self.user && self.group && self.other
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PermSpec {
    pub(crate) read: bool,
    pub(crate) write: bool,
    pub(crate) exec: bool,
    pub(crate) exec_if_conditional: bool,
    pub(crate) setuid: bool,
    pub(crate) setgid: bool,
    pub(crate) sticky: bool,
    pub(crate) copy_user: bool,
    pub(crate) copy_group: bool,
    pub(crate) copy_other: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SymbolicClause {
    pub(crate) target: TargetSelector,
    pub(crate) op: Operation,
    pub(crate) who: WhoMask,
    pub(crate) perms: PermSpec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NumericClause {
    pub(crate) target: TargetSelector,
    pub(crate) mode: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClauseKind {
    Symbolic(SymbolicClause),
    Numeric(NumericClause),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Clause {
    pub(crate) kind: ClauseKind,
}
