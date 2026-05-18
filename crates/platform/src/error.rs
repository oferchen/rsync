//! Typed error types for the [`platform`](crate) crate.
//!
//! These errors replace lossy `io::Error::new(io::ErrorKind::Other, ...)`
//! conversions at API boundaries. They preserve the original error via
//! [`std::error::Error::source`] so callers can downcast and inspect the
//! underlying cause (for example, a Windows HRESULT or `std::io::Error`).
//!
//! Each error implements `Into<io::Error>` so existing `io::Result`-returning
//! signatures stay unchanged: the typed error is attached as the `io::Error`
//! payload and remains reachable through `io::Error::get_ref().downcast_ref()`.

use std::io;

/// Errors raised by [`crate::signal::register_signal_handlers`] on Windows.
#[derive(Debug, thiserror::Error)]
pub enum SignalRegistrationError {
    /// Signal handler statics were already initialized in this process.
    ///
    /// Registration is one-shot per process; the second call fails.
    #[error("signal handlers already registered")]
    AlreadyRegistered,

    /// `SetConsoleCtrlHandler` rejected the registration.
    #[cfg(windows)]
    #[error("SetConsoleCtrlHandler failed")]
    SetConsoleCtrlHandlerFailed(#[source] windows::core::Error),
}

impl From<SignalRegistrationError> for io::Error {
    fn from(err: SignalRegistrationError) -> Self {
        io::Error::other(err)
    }
}

/// Errors raised by the Windows Service Control Manager helpers in
/// [`crate::windows_service`].
#[derive(Debug, thiserror::Error)]
pub enum WindowsServiceError {
    /// The SCM dispatcher was already started in this process.
    #[error("service dispatcher already initialized")]
    DispatcherAlreadyInitialized,

    /// `StartServiceCtrlDispatcherW` returned an error (typically because the
    /// process was not launched by the SCM).
    #[cfg(windows)]
    #[error("StartServiceCtrlDispatcherW failed")]
    StartDispatcherFailed(#[source] windows::core::Error),

    /// `SetServiceStatus` failed to publish the new state.
    #[cfg(windows)]
    #[error("SetServiceStatus failed")]
    SetServiceStatusFailed(#[source] windows::core::Error),

    /// `std::env::current_exe` failed while resolving the install path.
    #[error("failed to determine executable path")]
    CurrentExeFailed(#[source] io::Error),

    /// `CreateServiceW` rejected the registration request.
    #[cfg(windows)]
    #[error("failed to create service")]
    CreateServiceFailed(#[source] windows::core::Error),

    /// `DeleteService` failed to remove the named service.
    #[cfg(windows)]
    #[error("failed to delete service {name:?}")]
    DeleteServiceFailed {
        /// Name of the service that could not be removed.
        name: &'static str,
        /// Underlying Win32 error returned by `DeleteService`.
        #[source]
        source: windows::core::Error,
    },
}

impl From<WindowsServiceError> for io::Error {
    fn from(err: WindowsServiceError) -> Self {
        io::Error::other(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn signal_registration_already_registered_downcasts_from_io_error() {
        let err: io::Error = SignalRegistrationError::AlreadyRegistered.into();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        let inner = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<SignalRegistrationError>())
            .expect("inner error preserved");
        assert!(matches!(inner, SignalRegistrationError::AlreadyRegistered));
    }

    #[test]
    fn windows_service_dispatcher_initialized_downcasts() {
        let err: io::Error = WindowsServiceError::DispatcherAlreadyInitialized.into();
        let inner = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<WindowsServiceError>())
            .expect("inner error preserved");
        assert!(matches!(
            inner,
            WindowsServiceError::DispatcherAlreadyInitialized
        ));
    }

    #[test]
    fn windows_service_current_exe_preserves_source() {
        let cause = io::Error::new(io::ErrorKind::NotFound, "missing exe");
        let typed = WindowsServiceError::CurrentExeFailed(cause);
        let chained: io::Error = typed.into();
        let inner = chained
            .get_ref()
            .and_then(|e| e.downcast_ref::<WindowsServiceError>())
            .expect("typed error survives io::Error wrapping");
        let source = inner.source().expect("source chain preserved");
        assert!(source.to_string().contains("missing exe"));
    }
}
