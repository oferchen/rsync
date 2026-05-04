//! RAII wrapper around a Windows I/O Completion Port handle.
//!
//! A completion port is the central dispatching mechanism for overlapped I/O
//! on Windows. File handles are associated with the port, and completed I/O
//! operations are dequeued via `GetQueuedCompletionStatus`.

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::IO::CreateIoCompletionPort;

/// RAII wrapper for a Windows I/O Completion Port.
///
/// Automatically closes the port handle on drop. The port can have multiple
/// file handles associated with it; completed I/O operations from any
/// associated handle are dequeued through this port.
pub(crate) struct CompletionPort {
    handle: HANDLE,
}

impl CompletionPort {
    /// Creates a new I/O Completion Port.
    ///
    /// `max_threads` controls the maximum number of threads the OS allows
    /// to concurrently process completions. Pass 0 to use the number of
    /// processors.
    pub(crate) fn new(max_threads: u32) -> io::Result<Self> {
        // SAFETY: CreateIoCompletionPort with INVALID_HANDLE_VALUE creates a
        // new standalone completion port. The returned handle is valid until
        // CloseHandle is called.
        #[allow(unsafe_code)]
        let handle = unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, max_threads)
        };

        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { handle })
    }

    /// Associates a file handle with this completion port.
    ///
    /// `key` is a per-handle value returned with each completion event,
    /// allowing the caller to identify which file the completion belongs to.
    pub(crate) fn associate(&self, file_handle: HANDLE, key: usize) -> io::Result<()> {
        // SAFETY: Both handles are valid - self.handle is owned by this struct,
        // and file_handle is guaranteed valid by the caller. The association
        // persists until the file handle is closed.
        #[allow(unsafe_code)]
        let result = unsafe { CreateIoCompletionPort(file_handle, self.handle, key, 0) };

        if result.is_null() {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    /// Returns the raw completion port handle for use with Win32 APIs.
    pub(crate) fn handle(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for CompletionPort {
    fn drop(&mut self) {
        // SAFETY: self.handle is a valid handle obtained from
        // CreateIoCompletionPort and has not been closed yet.
        #[allow(unsafe_code)]
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

// SAFETY: The completion port handle is an opaque kernel object.
// Windows kernel objects are thread-safe - they can be used from any thread.
#[allow(unsafe_code)]
unsafe impl Send for CompletionPort {}

// SAFETY: Windows completion ports are designed for concurrent access.
// GetQueuedCompletionStatus can be called from multiple threads simultaneously.
#[allow(unsafe_code)]
unsafe impl Sync for CompletionPort {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_completion_port() {
        let port = CompletionPort::new(1).unwrap();
        assert!(!port.handle().is_null());
    }

    #[test]
    fn create_completion_port_zero_threads() {
        let port = CompletionPort::new(0).unwrap();
        assert!(!port.handle().is_null());
    }

    #[test]
    fn completion_port_drops_cleanly() {
        let port = CompletionPort::new(1).unwrap();
        drop(port);
    }
}
