#![allow(clippy::module_name_repetitions)]

//! Helpers for emitting `sd_notify` state transitions when the daemon is built
//! with the optional `sd-notify` feature. The notifier is intentionally thin:
//! it records whether `NOTIFY_SOCKET` was present at start-up and provides
//! convenience methods for the `READY=1`, `STATUS=...`, and `STOPPING=1` messages
//! used by the systemd service unit. When the feature is disabled the helpers
//! compile down to no-ops so the rest of the daemon can call them unconditionally.

use std::io;

#[cfg(all(feature = "sd-notify", target_os = "linux"))]
use sd_notify::NotifyState;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ServiceNotifier {
    #[cfg(all(feature = "sd-notify", target_os = "linux"))]
    available: bool,
}

// Allow trivially_copy_pass_by_ref: struct is ZST when sd-notify is disabled,
// but &self is idiomatic for methods and the struct has a field when enabled.
#[allow(clippy::trivially_copy_pass_by_ref)]
impl ServiceNotifier {
    /// Constructs a notifier that reports whether `sd_notify` integration is
    /// available. When the `sd-notify` feature is disabled or `NOTIFY_SOCKET`
    /// is absent the notifier becomes a no-op.
    #[must_use]
    pub(crate) fn new() -> Self {
        #[cfg(all(feature = "sd-notify", target_os = "linux"))]
        {
            let available = std::env::var_os("NOTIFY_SOCKET").is_some();
            Self { available }
        }

        #[cfg(not(all(feature = "sd-notify", target_os = "linux")))]
        {
            Self {}
        }
    }

    /// Reports service readiness to the init system.
    pub(crate) fn ready(&self, status: Option<&str>) -> io::Result<()> {
        #[cfg(all(feature = "sd-notify", target_os = "linux"))]
        {
            if let Some(text) = status {
                self.send_states(&[NotifyState::Ready, NotifyState::Status(text)])
            } else {
                self.send_states(&[NotifyState::Ready])
            }
        }

        #[cfg(not(all(feature = "sd-notify", target_os = "linux")))]
        {
            let _ = status;
            Ok(())
        }
    }

    /// Sends an updated status message.
    pub(crate) fn status(&self, status: &str) -> io::Result<()> {
        #[cfg(all(feature = "sd-notify", target_os = "linux"))]
        {
            self.send_states(&[NotifyState::Status(status)])
        }

        #[cfg(not(all(feature = "sd-notify", target_os = "linux")))]
        {
            let _ = status;
            Ok(())
        }
    }

    /// Indicates that the daemon is shutting down.
    pub(crate) fn stopping(&self) -> io::Result<()> {
        #[cfg(all(feature = "sd-notify", target_os = "linux"))]
        {
            self.send_states(&[NotifyState::Stopping])
        }

        #[cfg(not(all(feature = "sd-notify", target_os = "linux")))]
        {
            Ok(())
        }
    }

    #[cfg(all(feature = "sd-notify", target_os = "linux"))]
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
    use crate::test_env::{ENV_LOCK, EnvGuard};

    #[test]
    fn notifier_behaves_as_noop_without_notify_socket() {
        let _env_lock = ENV_LOCK.lock().expect("lock environment guard");
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        assert_eq!(notifier, ServiceNotifier::default());
        assert!(notifier.ready(Some("Listening on 127.0.0.1:873")).is_ok());
        assert!(notifier.ready(None).is_ok());
        assert!(notifier.status("serving 0 connections").is_ok());
        assert!(notifier.stopping().is_ok());
    }

    #[test]
    fn notifier_default_eq() {
        let a = ServiceNotifier::default();
        let b = ServiceNotifier::default();
        assert_eq!(a, b);
    }

    #[test]
    fn notifier_clone() {
        let notifier = ServiceNotifier::new();
        let cloned = notifier;
        assert_eq!(notifier, cloned);
    }

    #[test]
    fn notifier_debug() {
        let notifier = ServiceNotifier::new();
        let debug = format!("{notifier:?}");
        assert!(debug.contains("ServiceNotifier"));
    }

    #[test]
    fn notifier_ready_with_status_succeeds() {
        let _env_lock = ENV_LOCK.lock().expect("lock environment guard");
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        assert!(notifier.ready(Some("status message")).is_ok());
    }

    #[test]
    fn notifier_ready_without_status_succeeds() {
        let _env_lock = ENV_LOCK.lock().expect("lock environment guard");
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        assert!(notifier.ready(None).is_ok());
    }

    #[test]
    fn notifier_status_with_empty_message_succeeds() {
        let _env_lock = ENV_LOCK.lock().expect("lock environment guard");
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        assert!(notifier.status("").is_ok());
    }

    #[test]
    fn notifier_status_with_long_message_succeeds() {
        let _env_lock = ENV_LOCK.lock().expect("lock environment guard");
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        let long_message = "x".repeat(1000);
        assert!(notifier.status(&long_message).is_ok());
    }

    #[test]
    fn notifier_multiple_status_updates_succeed() {
        let _env_lock = ENV_LOCK.lock().expect("lock environment guard");
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        for i in 0..10 {
            assert!(notifier.status(&format!("update {i}")).is_ok());
        }
    }

    #[test]
    fn notifier_stopping_multiple_times_succeeds() {
        let _env_lock = ENV_LOCK.lock().expect("lock environment guard");
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        // Multiple stopping calls should not fail
        assert!(notifier.stopping().is_ok());
        assert!(notifier.stopping().is_ok());
    }

    #[test]
    fn notifier_full_lifecycle() {
        let _env_lock = ENV_LOCK.lock().expect("lock environment guard");
        let _guard = EnvGuard::remove("NOTIFY_SOCKET");

        let notifier = ServiceNotifier::new();
        // Simulate typical daemon lifecycle
        assert!(notifier.ready(Some("Listening on port 873")).is_ok());
        assert!(notifier.status("Active connections: 0").is_ok());
        assert!(notifier.status("Active connections: 5").is_ok());
        assert!(notifier.status("Shutting down...").is_ok());
        assert!(notifier.stopping().is_ok());
    }
}
