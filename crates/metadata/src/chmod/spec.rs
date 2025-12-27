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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_selector_clone() {
        let selector = TargetSelector::Files;
        let cloned = selector;
        assert_eq!(selector, cloned);
    }

    #[test]
    fn target_selector_debug() {
        let selector = TargetSelector::All;
        let debug = format!("{selector:?}");
        assert!(debug.contains("All"));
    }

    #[test]
    fn target_selector_eq() {
        assert_eq!(TargetSelector::All, TargetSelector::All);
        assert_eq!(TargetSelector::Files, TargetSelector::Files);
        assert_eq!(TargetSelector::Directories, TargetSelector::Directories);
        assert_ne!(TargetSelector::All, TargetSelector::Files);
    }

    #[test]
    fn operation_clone() {
        let op = Operation::Add;
        let cloned = op;
        assert_eq!(op, cloned);
    }

    #[test]
    fn operation_debug() {
        assert!(format!("{:?}", Operation::Add).contains("Add"));
        assert!(format!("{:?}", Operation::Remove).contains("Remove"));
        assert!(format!("{:?}", Operation::Assign).contains("Assign"));
    }

    #[test]
    fn operation_eq() {
        assert_eq!(Operation::Add, Operation::Add);
        assert_ne!(Operation::Add, Operation::Remove);
        assert_ne!(Operation::Remove, Operation::Assign);
    }

    #[test]
    fn who_mask_new() {
        let mask = WhoMask::new(true, false, true);
        assert!(mask.user);
        assert!(!mask.group);
        assert!(mask.other);
    }

    #[test]
    fn who_mask_includes_user() {
        let mask = WhoMask::new(true, false, false);
        assert!(mask.includes_user());
        assert!(!mask.includes_group());
        assert!(!mask.includes_other());
    }

    #[test]
    fn who_mask_includes_group() {
        let mask = WhoMask::new(false, true, false);
        assert!(!mask.includes_user());
        assert!(mask.includes_group());
        assert!(!mask.includes_other());
    }

    #[test]
    fn who_mask_includes_other() {
        let mask = WhoMask::new(false, false, true);
        assert!(!mask.includes_user());
        assert!(!mask.includes_group());
        assert!(mask.includes_other());
    }

    #[test]
    fn who_mask_covers_all_true() {
        let mask = WhoMask::new(true, true, true);
        assert!(mask.covers_all());
    }

    #[test]
    fn who_mask_covers_all_false() {
        let mask = WhoMask::new(true, true, false);
        assert!(!mask.covers_all());
    }

    #[test]
    fn perm_spec_default() {
        let spec = PermSpec::default();
        assert!(!spec.read);
        assert!(!spec.write);
        assert!(!spec.exec);
        assert!(!spec.exec_if_conditional);
        assert!(!spec.setuid);
        assert!(!spec.setgid);
        assert!(!spec.sticky);
        assert!(!spec.copy_user);
        assert!(!spec.copy_group);
        assert!(!spec.copy_other);
    }

    #[test]
    fn perm_spec_clone() {
        let spec = PermSpec {
            read: true,
            write: true,
            ..Default::default()
        };
        let cloned = spec;
        assert_eq!(spec, cloned);
    }

    #[test]
    fn symbolic_clause_clone() {
        let clause = SymbolicClause {
            target: TargetSelector::All,
            op: Operation::Add,
            who: WhoMask::new(true, true, true),
            perms: PermSpec::default(),
        };
        let cloned = clause.clone();
        assert_eq!(clause, cloned);
    }

    #[test]
    fn numeric_clause_clone() {
        let clause = NumericClause {
            target: TargetSelector::Files,
            mode: 0o755,
        };
        let cloned = clause.clone();
        assert_eq!(clause, cloned);
    }

    #[test]
    fn clause_kind_symbolic() {
        let kind = ClauseKind::Symbolic(SymbolicClause {
            target: TargetSelector::All,
            op: Operation::Assign,
            who: WhoMask::new(true, false, false),
            perms: PermSpec::default(),
        });
        assert!(matches!(kind, ClauseKind::Symbolic(_)));
    }

    #[test]
    fn clause_kind_numeric() {
        let kind = ClauseKind::Numeric(NumericClause {
            target: TargetSelector::All,
            mode: 0o644,
        });
        assert!(matches!(kind, ClauseKind::Numeric(_)));
    }

    #[test]
    fn clause_clone() {
        let clause = Clause {
            kind: ClauseKind::Numeric(NumericClause {
                target: TargetSelector::All,
                mode: 0o777,
            }),
        };
        let cloned = clause.clone();
        assert_eq!(clause, cloned);
    }
}
