use std::num::NonZeroU64;
use std::time::Duration;

/// Timeout configuration for network operations.
///
/// Higher layers resolve this into a concrete [`Duration`] (or `None` to
/// disable) via [`TransferTimeout::effective`], supplying a transport-specific
/// default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum TransferTimeout {
    /// Defer to the caller-supplied default.
    #[default]
    Default,
    /// Disable socket timeouts entirely.
    Disabled,
    /// Explicit timeout in seconds. `NonZeroU64` rules out zero, which upstream
    /// rsync treats as "no timeout" rather than "expire immediately"
    /// (upstream: io.c:set_io_timeout, options.c:--no-timeout).
    Seconds(NonZeroU64),
}

impl TransferTimeout {
    /// Resolves to a concrete [`Duration`], or `None` when timeouts are disabled.
    ///
    /// `default` is used only for the [`TransferTimeout::Default`] variant.
    pub const fn effective(self, default: Duration) -> Option<Duration> {
        match self {
            TransferTimeout::Default => Some(default),
            TransferTimeout::Disabled => None,
            TransferTimeout::Seconds(seconds) => Some(Duration::from_secs(seconds.get())),
        }
    }

    /// Returns the configured seconds value, or `None` for `Default`/`Disabled`.
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
