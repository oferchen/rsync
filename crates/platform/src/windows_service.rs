//! Windows Service Control Manager (SCM) integration.
//!
//! Provides safe wrappers around Win32 service APIs for running the daemon as a
//! Windows service. The unsafe FFI calls are scoped to individual functions with
//! detailed safety comments.
//!
//! # Architecture
//!
//! - [`run_service_dispatcher`] starts the SCM service dispatch table, blocking
//!   until the service stops.
//! - [`ServiceStatusHandle`] reports lifecycle transitions to the SCM.
//! - [`install_service`] and [`uninstall_service`] manage service registration.
//! - The control handler maps SCM events to [`signal::SignalFlags`] atomics,
//!   reusing the same shutdown/reload mechanism as console mode.

use std::io;

use crate::signal::SignalFlags;

/// Service name used for SCM registration.
pub const SERVICE_NAME: &str = "oc-rsync";

/// Display name shown in the Windows Services management console.
pub const SERVICE_DISPLAY_NAME: &str = "oc-rsync daemon";

/// Description shown in the Windows Services management console.
pub const SERVICE_DESCRIPTION: &str = "Pure-Rust rsync-compatible file synchronization daemon";

/// Callback invoked by the SCM dispatcher to start the service.
///
/// The callback receives [`SignalFlags`] that are wired to the SCM control
/// handler, and must run the daemon accept loop. When the callback returns,
/// the service reports `SERVICE_STOPPED`.
pub type ServiceMainCallback = Box<dyn FnOnce(SignalFlags) -> Result<(), io::Error> + Send>;

/// Opaque handle for reporting service status to the SCM.
///
/// On non-Windows this is a no-op stub so daemon code can call it
/// unconditionally.
#[derive(Debug, Clone)]
pub struct ServiceStatusHandle {
    #[cfg(windows)]
    inner: windows::Win32::System::Services::SERVICE_STATUS_HANDLE,
}

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::OnceLock;
    use std::sync::atomic::Ordering;

    use windows::Win32::System::Services::{
        CloseServiceHandle, CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW,
        RegisterServiceCtrlHandlerW, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_AUTO_START,
        SERVICE_CONTROL_PARAMCHANGE, SERVICE_CONTROL_PRESHUTDOWN, SERVICE_CONTROL_SHUTDOWN,
        SERVICE_CONTROL_STOP, SERVICE_ERROR_NORMAL, SERVICE_RUNNING, SERVICE_START_PENDING,
        SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOP_PENDING, SERVICE_STOPPED,
        SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
        StartServiceCtrlDispatcherW,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::{SERVICE_DISPLAY_NAME, SERVICE_NAME, ServiceMainCallback, ServiceStatusHandle};
    use crate::signal::SignalFlags;

    /// Win32 error code indicating a service-specific exit code is present
    /// in `dwServiceSpecificExitCode`.
    const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

    /// Wrapper for `SERVICE_STATUS_HANDLE` that implements Send + Sync.
    ///
    /// `SERVICE_STATUS_HANDLE` is an opaque `*mut c_void` that Windows
    /// guarantees valid for the process lifetime once obtained from
    /// `RegisterServiceCtrlHandlerW`. The SCM dispatches control events
    /// from arbitrary threads, so the handle must be shareable.
    #[derive(Clone, Copy)]
    struct SendSyncHandle(windows::Win32::System::Services::SERVICE_STATUS_HANDLE);

    // SAFETY: SERVICE_STATUS_HANDLE is a Windows kernel handle (opaque
    // *mut c_void). It is set once during RegisterServiceCtrlHandlerW and
    // remains valid for the process lifetime. The SCM itself calls the
    // control handler from different threads, so the handle is thread-safe
    // by design.
    #[allow(unsafe_code)]
    unsafe impl Send for SendSyncHandle {}
    #[allow(unsafe_code)]
    unsafe impl Sync for SendSyncHandle {}

    // Global state shared between the service dispatcher callback and the
    // control handler. OnceLock ensures one-time initialization.
    static SERVICE_FLAGS: OnceLock<SignalFlags> = OnceLock::new();
    static SERVICE_STATUS_HANDLE: OnceLock<SendSyncHandle> = OnceLock::new();
    static SERVICE_CALLBACK: OnceLock<std::sync::Mutex<Option<ServiceMainCallback>>> =
        OnceLock::new();

    /// Encodes a Rust string as a null-terminated UTF-16 vector for Win32 APIs.
    fn to_wide_null(s: &str) -> Vec<u16> {
        OsString::from(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Starts the SCM service control dispatcher.
    ///
    /// This function blocks until the service stops. The provided callback is
    /// invoked from the SCM's service thread with [`SignalFlags`] wired to the
    /// control handler.
    ///
    /// # Errors
    ///
    /// Returns an error if the service dispatcher fails to start (e.g., the
    /// process was not launched by the SCM).
    #[allow(unsafe_code)]
    pub fn run_service_dispatcher(callback: ServiceMainCallback) -> Result<(), io::Error> {
        SERVICE_CALLBACK
            .set(std::sync::Mutex::new(Some(callback)))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::Other,
                    "service dispatcher already initialized",
                )
            })?;

        let service_name = to_wide_null(SERVICE_NAME);

        let service_table = [
            SERVICE_TABLE_ENTRYW {
                lpServiceName: PWSTR(service_name.as_ptr() as *mut u16),
                lpServiceProc: Some(service_main_entry),
            },
            // Null terminator entry.
            SERVICE_TABLE_ENTRYW {
                lpServiceName: PWSTR(std::ptr::null_mut()),
                lpServiceProc: None,
            },
        ];

        // SAFETY: service_table is a valid null-terminated array of
        // SERVICE_TABLE_ENTRYW. The callback function signature matches the
        // LPSERVICE_MAIN_FUNCTIONW type. The service_name vector lives for the
        // duration of this call.
        unsafe { StartServiceCtrlDispatcherW(service_table.as_ptr()) }.map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("StartServiceCtrlDispatcherW failed: {e}"),
            )
        })?;

        Ok(())
    }

    /// SCM service main entry point.
    ///
    /// Called by the SCM dispatcher thread. Registers the control handler,
    /// reports `SERVICE_START_PENDING`, then invokes the user callback.
    #[allow(unsafe_code)]
    unsafe extern "system" fn service_main_entry(_argc: u32, _argv: *mut windows::core::PWSTR) {
        let service_name = to_wide_null(SERVICE_NAME);

        // Register the control handler before doing anything else.
        // SAFETY: service_name is a valid null-terminated UTF-16 string.
        // handler_function matches the LPHANDLER_FUNCTION signature.
        let status_handle = match unsafe {
            RegisterServiceCtrlHandlerW(
                PCWSTR(service_name.as_ptr()),
                Some(service_control_handler),
            )
        } {
            Ok(handle) => handle,
            Err(_) => return,
        };

        let _ = SERVICE_STATUS_HANDLE.set(SendSyncHandle(status_handle));

        // Publish flags globally so the control handler can reach them.
        let flags = SignalFlags::new();
        let _ = SERVICE_FLAGS.set(flags.clone());

        let _ = report_status_raw(status_handle, SERVICE_START_PENDING, 0, 3000);
        let _ = report_status_raw(status_handle, SERVICE_RUNNING, 0, 0);

        let callback = SERVICE_CALLBACK
            .get()
            .and_then(|mutex| mutex.lock().ok())
            .and_then(|mut guard| guard.take());

        let exit_code = match callback {
            Some(cb) => cb(flags).map_or(1, |()| 0),
            None => 1,
        };

        let _ = report_status_raw(status_handle, SERVICE_STOPPED, exit_code, 0);
    }

    /// SCM control handler callback.
    ///
    /// Maps SCM control codes to [`SignalFlags`] atomics, mirroring the Unix
    /// signal handler approach.
    ///
    /// - `SERVICE_CONTROL_STOP` / `SERVICE_CONTROL_SHUTDOWN` -> `shutdown`
    /// - `SERVICE_CONTROL_PARAMCHANGE` -> `reload_config`
    /// - `SERVICE_CONTROL_PRESHUTDOWN` -> `graceful_exit`
    #[allow(unsafe_code)]
    unsafe extern "system" fn service_control_handler(control: u32) {
        if let Some(flags) = SERVICE_FLAGS.get() {
            match control {
                x if x == SERVICE_CONTROL_STOP || x == SERVICE_CONTROL_SHUTDOWN => {
                    flags.shutdown.store(true, Ordering::Relaxed);
                    // Report stop pending so SCM knows we're shutting down.
                    if let Some(handle) = SERVICE_STATUS_HANDLE.get() {
                        let _ = report_status_raw(handle.0, SERVICE_STOP_PENDING, 0, 5000);
                    }
                }
                x if x == SERVICE_CONTROL_PARAMCHANGE => {
                    flags.reload_config.store(true, Ordering::Relaxed);
                }
                x if x == SERVICE_CONTROL_PRESHUTDOWN => {
                    flags.graceful_exit.store(true, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }

    /// Reports a raw service status to the SCM.
    #[allow(unsafe_code)]
    fn report_status_raw(
        handle: windows::Win32::System::Services::SERVICE_STATUS_HANDLE,
        state: SERVICE_STATUS_CURRENT_STATE,
        exit_code: u32,
        wait_hint: u32,
    ) -> Result<(), io::Error> {
        let mut accepted = windows::Win32::System::Services::SERVICE_ACCEPT_STOP
            | windows::Win32::System::Services::SERVICE_ACCEPT_SHUTDOWN
            | windows::Win32::System::Services::SERVICE_ACCEPT_PARAMCHANGE
            | windows::Win32::System::Services::SERVICE_ACCEPT_PRESHUTDOWN;

        // When stopped or stop-pending, don't accept any controls.
        if state == SERVICE_STOPPED || state == SERVICE_STOP_PENDING {
            accepted = 0;
        }

        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: accepted,
            dwWin32ExitCode: if exit_code == 0 {
                0
            } else {
                ERROR_SERVICE_SPECIFIC_ERROR
            },
            dwServiceSpecificExitCode: exit_code,
            dwCheckPoint: 0,
            dwWaitHint: wait_hint,
        };

        // SAFETY: handle is a valid SERVICE_STATUS_HANDLE obtained from
        // RegisterServiceCtrlHandlerW. status is a properly initialized struct.
        unsafe { SetServiceStatus(handle, &status) }.map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("SetServiceStatus failed: {e}"),
            )
        })?;

        Ok(())
    }

    /// Registers the service with the Windows SCM.
    ///
    /// Creates a service entry pointing to the current executable with the
    /// `--daemon --windows-service` arguments.
    ///
    /// # Errors
    ///
    /// Returns an error if SCM access or service creation fails (e.g.,
    /// insufficient privileges).
    #[allow(unsafe_code)]
    pub fn install_service() -> Result<(), io::Error> {
        let exe_path = std::env::current_exe().map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("failed to determine executable path: {e}"),
            )
        })?;

        let binary_path = format!("\"{}\" --daemon --windows-service", exe_path.display());
        let binary_path_wide = to_wide_null(&binary_path);
        let service_name_wide = to_wide_null(SERVICE_NAME);
        let display_name_wide = to_wide_null(SERVICE_DISPLAY_NAME);

        // SAFETY: Passing null for machine name opens the local SCM database.
        // SC_MANAGER_ALL_ACCESS provides full access for service creation.
        let scm = unsafe {
            OpenSCManagerW(
                PCWSTR(std::ptr::null()),
                PCWSTR(std::ptr::null()),
                SC_MANAGER_ALL_ACCESS,
            )
        }
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("failed to open SCM (run as Administrator): {e}"),
            )
        })?;

        // SAFETY: scm is a valid SC_HANDLE from OpenSCManagerW. All string
        // pointers are valid null-terminated UTF-16. The service is created as
        // auto-start with no dependencies.
        let service_result = unsafe {
            CreateServiceW(
                scm,
                PCWSTR(service_name_wide.as_ptr()),
                PCWSTR(display_name_wide.as_ptr()),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                PCWSTR(binary_path_wide.as_ptr()),
                PCWSTR(std::ptr::null()), // no load ordering group
                None,                     // no tag
                PCWSTR(std::ptr::null()), // no dependencies
                PCWSTR(std::ptr::null()), // LocalSystem account
                PCWSTR(std::ptr::null()), // no password
            )
        };

        let service = match service_result {
            Ok(handle) => handle,
            Err(e) => {
                // SAFETY: scm is a valid SC_HANDLE.
                unsafe {
                    let _ = CloseServiceHandle(scm);
                }
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("failed to create service: {e}"),
                ));
            }
        };

        // SAFETY: service and scm are valid SC_HANDLEs.
        unsafe {
            let _ = CloseServiceHandle(service);
            let _ = CloseServiceHandle(scm);
        }

        Ok(())
    }

    /// Removes the service from the Windows SCM.
    ///
    /// The service must be stopped before it can be deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if SCM access or service deletion fails.
    #[allow(unsafe_code)]
    pub fn uninstall_service() -> Result<(), io::Error> {
        let service_name_wide = to_wide_null(SERVICE_NAME);

        // SAFETY: Passing null for machine name opens the local SCM database.
        let scm = unsafe {
            OpenSCManagerW(
                PCWSTR(std::ptr::null()),
                PCWSTR(std::ptr::null()),
                SC_MANAGER_ALL_ACCESS,
            )
        }
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("failed to open SCM (run as Administrator): {e}"),
            )
        })?;

        // SAFETY: scm is a valid SC_HANDLE, service_name_wide is null-terminated.
        let service_result =
            unsafe { OpenServiceW(scm, PCWSTR(service_name_wide.as_ptr()), SERVICE_ALL_ACCESS) };

        let service = match service_result {
            Ok(handle) => handle,
            Err(e) => {
                unsafe {
                    let _ = CloseServiceHandle(scm);
                }
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("failed to open service '{SERVICE_NAME}': {e}"),
                ));
            }
        };

        // SAFETY: service is a valid SC_HANDLE from OpenServiceW.
        let delete_result = unsafe { DeleteService(service) };

        // SAFETY: service and scm are valid SC_HANDLEs.
        unsafe {
            let _ = CloseServiceHandle(service);
            let _ = CloseServiceHandle(scm);
        }

        delete_result.map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("failed to delete service '{SERVICE_NAME}': {e}"),
            )
        })?;

        Ok(())
    }

    impl ServiceStatusHandle {
        /// Creates a handle from the global SCM status handle.
        ///
        /// Returns `None` if the service dispatcher has not been started.
        pub fn from_global() -> Option<Self> {
            SERVICE_STATUS_HANDLE
                .get()
                .map(|wrapper| Self { inner: wrapper.0 })
        }

        /// Reports `SERVICE_RUNNING` to the SCM.
        pub fn report_running(&self) -> Result<(), io::Error> {
            report_status_raw(self.inner, SERVICE_RUNNING, 0, 0)
        }

        /// Reports `SERVICE_STOP_PENDING` to the SCM.
        pub fn report_stopping(&self) -> Result<(), io::Error> {
            report_status_raw(self.inner, SERVICE_STOP_PENDING, 0, 5000)
        }

        /// Reports `SERVICE_STOPPED` to the SCM.
        pub fn report_stopped(&self, exit_code: u32) -> Result<(), io::Error> {
            report_status_raw(self.inner, SERVICE_STOPPED, exit_code, 0)
        }
    }
}

// Non-Windows stubs so the daemon crate can reference these types and functions
// unconditionally.
#[cfg(not(windows))]
mod non_windows_impl {
    use std::io;

    use super::{ServiceMainCallback, ServiceStatusHandle};

    /// Returns an error on non-Windows platforms.
    pub fn run_service_dispatcher(_callback: ServiceMainCallback) -> Result<(), io::Error> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows Service mode is only available on Windows",
        ))
    }

    /// Returns an error on non-Windows platforms.
    pub fn install_service() -> Result<(), io::Error> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "--install-service is only available on Windows",
        ))
    }

    /// Returns an error on non-Windows platforms.
    pub fn uninstall_service() -> Result<(), io::Error> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "--uninstall-service is only available on Windows",
        ))
    }

    impl ServiceStatusHandle {
        /// Always returns `None` on non-Windows.
        pub fn from_global() -> Option<Self> {
            None
        }

        /// No-op on non-Windows.
        pub fn report_running(&self) -> Result<(), io::Error> {
            Ok(())
        }

        /// No-op on non-Windows.
        pub fn report_stopping(&self) -> Result<(), io::Error> {
            Ok(())
        }

        /// No-op on non-Windows.
        pub fn report_stopped(&self, _exit_code: u32) -> Result<(), io::Error> {
            Ok(())
        }
    }
}

#[cfg(windows)]
pub use windows_impl::{install_service, run_service_dispatcher, uninstall_service};

#[cfg(not(windows))]
pub use non_windows_impl::{install_service, run_service_dispatcher, uninstall_service};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_status_handle_from_global_returns_none_without_dispatcher() {
        // Without starting the service dispatcher, there's no global handle.
        assert!(ServiceStatusHandle::from_global().is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn install_service_fails_on_non_windows() {
        let result = install_service();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(not(windows))]
    #[test]
    fn uninstall_service_fails_on_non_windows() {
        let result = uninstall_service();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(not(windows))]
    #[test]
    fn run_service_dispatcher_fails_on_non_windows() {
        let callback: ServiceMainCallback = Box::new(|_flags| Ok(()));
        let result = run_service_dispatcher(callback);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }
}
