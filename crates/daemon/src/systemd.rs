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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
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
            if let Some(text) = status {
                return self.send_states(&[NotifyState::Ready, NotifyState::Status(text)]);
            }

            return self.send_states(&[NotifyState::Ready]);
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
            return self.send_states(&[NotifyState::Status(status)]);
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
            return self.send_states(&[NotifyState::Stopping]);
        }

        #[cfg(not(feature = "sd-notify"))]
        {
            Ok(())
        }
    }

    #[cfg(feature = "sd-notify")]
    fn send_states(&self, states: &[NotifyState]) -> io::Result<()> {
        if !self.available {
            return Ok(());
        }

        sd_notify::notify(false, states)
    }
}

#[cfg(test)]
mod tests {
    use super::ServiceNotifier;
    use std::env;
    use std::ffi::OsString;

    #[allow(unsafe_code)]
    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    #[allow(unsafe_code)]
    impl EnvGuard {
        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    #[allow(unsafe_code)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(ref value) = self.previous {
                unsafe {
                    env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn notifier_behaves_as_noop_without_notify_socket() {
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        assert_eq!(notifier, ServiceNotifier::default());
        assert!(notifier.ready(Some("Listening on 127.0.0.1:873")).is_ok());
        assert!(notifier.ready(None).is_ok());
        assert!(notifier.status("serving 0 connections").is_ok());
        assert!(notifier.stopping().is_ok());
    }
}
