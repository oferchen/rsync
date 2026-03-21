/// Deletion scheduling selected by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum DeleteMode {
    /// Do not remove extraneous destination entries.
    #[default]
    Disabled,
    /// Remove extraneous entries before transferring file data.
    Before,
    /// Remove extraneous entries while processing directory contents (upstream default).
    During,
    /// Record deletions during the walk and prune entries after transfers finish.
    Delay,
    /// Remove extraneous entries after the transfer completes.
    After,
}

impl DeleteMode {
    /// Returns `true` when deletion sweeps are enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        assert_eq!(DeleteMode::default(), DeleteMode::Disabled);
    }

    #[test]
    fn is_enabled_disabled() {
        assert!(!DeleteMode::Disabled.is_enabled());
    }

    #[test]
    fn is_enabled_before() {
        assert!(DeleteMode::Before.is_enabled());
    }

    #[test]
    fn is_enabled_during() {
        assert!(DeleteMode::During.is_enabled());
    }

    #[test]
    fn is_enabled_delay() {
        assert!(DeleteMode::Delay.is_enabled());
    }

    #[test]
    fn is_enabled_after() {
        assert!(DeleteMode::After.is_enabled());
    }

    #[test]
    fn clone_and_copy() {
        let mode = DeleteMode::Before;
        let cloned = mode;
        let copied = mode;
        assert_eq!(mode, cloned);
        assert_eq!(mode, copied);
    }

    #[test]
    fn debug_format() {
        assert_eq!(format!("{:?}", DeleteMode::Disabled), "Disabled");
        assert_eq!(format!("{:?}", DeleteMode::Before), "Before");
        assert_eq!(format!("{:?}", DeleteMode::During), "During");
        assert_eq!(format!("{:?}", DeleteMode::Delay), "Delay");
        assert_eq!(format!("{:?}", DeleteMode::After), "After");
    }
}
