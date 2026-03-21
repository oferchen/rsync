use std::num::NonZeroU64;
use std::time::Duration;

/// Describes the timeout configuration applied to network operations.
///
/// The variant captures whether the caller requested a custom timeout, disabled
/// socket timeouts entirely, or asked to rely on the default for the current
/// operation. Higher layers convert the setting into concrete [`Duration`]
/// values depending on the transport in use.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum TransferTimeout {
    /// Use the default timeout for the current operation.
    #[default]
    Default,
    /// Disable socket timeouts entirely.
    Disabled,
    /// Apply a caller-provided timeout expressed in seconds.
    Seconds(NonZeroU64),
}

impl TransferTimeout {
    /// Returns the timeout expressed as a [`Duration`] using the provided
    /// default when the setting is [`TransferTimeout::Default`].
    pub const fn effective(self, default: Duration) -> Option<Duration> {
        match self {
            TransferTimeout::Default => Some(default),
            TransferTimeout::Disabled => None,
            TransferTimeout::Seconds(seconds) => Some(Duration::from_secs(seconds.get())),
        }
    }

    /// Convenience helper returning the raw seconds value when specified.
    pub const fn as_seconds(self) -> Option<NonZeroU64> {
        match self {
            TransferTimeout::Seconds(value) => Some(value),
            TransferTimeout::Default | TransferTimeout::Disabled => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_default_variant() {
        let timeout = TransferTimeout::default();
        assert_eq!(timeout, TransferTimeout::Default);
    }

    #[test]
    fn effective_with_default() {
        let timeout = TransferTimeout::Default;
        let default = Duration::from_secs(30);
        assert_eq!(timeout.effective(default), Some(default));
    }

    #[test]
    fn effective_with_disabled() {
        let timeout = TransferTimeout::Disabled;
        let default = Duration::from_secs(30);
        assert_eq!(timeout.effective(default), None);
    }

    #[test]
    fn effective_with_seconds() {
        let timeout = TransferTimeout::Seconds(NonZeroU64::new(60).unwrap());
        let default = Duration::from_secs(30);
        assert_eq!(timeout.effective(default), Some(Duration::from_secs(60)));
    }

    #[test]
    fn as_seconds_with_seconds() {
        let timeout = TransferTimeout::Seconds(NonZeroU64::new(45).unwrap());
        assert_eq!(timeout.as_seconds(), Some(NonZeroU64::new(45).unwrap()));
    }

    #[test]
    fn as_seconds_with_default() {
        let timeout = TransferTimeout::Default;
        assert_eq!(timeout.as_seconds(), None);
    }

    #[test]
    fn as_seconds_with_disabled() {
        let timeout = TransferTimeout::Disabled;
        assert_eq!(timeout.as_seconds(), None);
    }

    #[test]
    fn clone_and_copy() {
        let timeout = TransferTimeout::Seconds(NonZeroU64::new(10).unwrap());
        let cloned = timeout;
        let copied = timeout;
        assert_eq!(timeout, cloned);
        assert_eq!(timeout, copied);
    }

    #[test]
    fn debug_format() {
        let timeout = TransferTimeout::Default;
        assert_eq!(format!("{timeout:?}"), "Default");

        let timeout = TransferTimeout::Disabled;
        assert_eq!(format!("{timeout:?}"), "Disabled");

        let timeout = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
        assert!(format!("{timeout:?}").contains("Seconds"));
    }
}
