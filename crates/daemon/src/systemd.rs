#![allow(clippy::module_name_repetitions)]

//! Helpers for emitting `sd_notify` state transitions when the daemon is built
//! with the optional `sd-notify` feature. The notifier is intentionally thin:
//! it records whether `NOTIFY_SOCKET` was present at start-up and provides
//! convenience methods for the `READY=1`, `STATUS=...`, and `STOPPING=1` messages
//! used by the systemd service unit. When the feature is disabled the helpers
//! compile down to no-ops so the rest of the daemon can call them unconditionally.

use std::io;

#[cfg(feature = "sd-notify")]
use sd_notify::NotifyState;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ServiceNotifier {
    #[cfg(feature = "sd-notify")]
    available: bool,
}

impl ServiceNotifier {
    /// Constructs a notifier that reports whether `sd_notify` integration is
    /// available. When the `sd-notify` feature is disabled or `NOTIFY_SOCKET`
    /// is absent the notifier becomes a no-op.
    #[must_use]
    pub(crate) fn new() -> Self {
        #[cfg(feature = "sd-notify")]
        {
            let available = std::env::var_os("NOTIFY_SOCKET").is_some();
            Self { available }
        }

        #[cfg(not(feature = "sd-notify"))]
        {
            Self {}
        }
    }

    /// Reports service readiness to the init system.
    pub(crate) fn ready(&self, status: Option<&str>) -> io::Result<()> {
        #[cfg(feature = "sd-notify")]
        {
            if !self.available {
                return Ok(());
            }

            if let Some(text) = status {
                sd_notify::notify(false, &[NotifyState::Ready, NotifyState::Status(text)])
            } else {
                sd_notify::notify(false, &[NotifyState::Ready])
            }
        }

        #[cfg(not(feature = "sd-notify"))]
        {
            let _ = status;
            Ok(())
        }
    }

    /// Sends an updated status message.
    pub(crate) fn status(&self, status: &str) -> io::Result<()> {
        #[cfg(feature = "sd-notify")]
        {
            if !self.available {
                return Ok(());
            }

            sd_notify::notify(false, &[NotifyState::Status(status)])
        }

        #[cfg(not(feature = "sd-notify"))]
        {
            let _ = status;
            Ok(())
        }
    }

    /// Indicates that the daemon is shutting down.
    pub(crate) fn stopping(&self) -> io::Result<()> {
        #[cfg(feature = "sd-notify")]
        {
            if !self.available {
                return Ok(());
            }

            sd_notify::notify(false, &[NotifyState::Stopping])
        }

        #[cfg(not(feature = "sd-notify"))]
        {
            Ok(())
        }
    }
}
