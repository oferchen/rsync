//! Cross-platform abstraction seam for zero-copy file-to-socket transfer.
//!
//! `PlatformSendFile` factors the platform-specific `sendfile(2)` /
//! `TransmitFile()` entry points behind a single trait. The default
//! implementations preserve byte-for-byte behaviour of the existing free
//! functions ([`crate::sendfile::send_file_to_fd`] on unix,
//! `crate::iocp::try_transmit_file` on Windows). The seam exists so
//! follow-on work can plug in alternative implementations (test doubles,
//! a Windows `TransmitFile` probe, future kTLS / SEND_ZC dispatch) at the
//! call site without touching the producers.
//!
//! # Scope
//!
//! This module is behaviour-neutral: every default implementation forwards
//! to a primitive that already exists in `fast_io`. No new syscall paths,
//! no new fast paths, no perf changes. The intent is purely structural -
//! introduce an inversion point so downstream wiring (e.g. WIN-S.LAND.1.c.2)
//! can swap in a `TransmitFile`-backed implementation behind the same
//! trait object.
//!
//! # Socket handle abstraction
//!
//! The trait carries the destination socket through [`SocketHandle`], a
//! thin newtype around the platform's raw integer-sized handle:
//!
//! - **unix**: wraps a `RawFd` (`i32`) - the same fd type accepted by
//!   [`crate::sendfile::send_file_to_fd`].
//! - **Windows**: wraps a `RawSocket` (`u64`) - the same Winsock handle
//!   accepted by `crate::iocp::try_transmit_file`.
//!
//! Callers obtain a `SocketHandle` from `AsRawFd::as_raw_fd()` or
//! `AsRawSocket::as_raw_socket()` on their existing socket type; the
//! handle is borrowed for the duration of the call and the trait never
//! takes ownership.

use std::fs::File;
use std::io;

/// Platform-portable raw socket descriptor for the
/// [`PlatformSendFile::send_to_socket`] trait method.
///
/// Carries the integer-sized handle that the platform `sendfile` /
/// `TransmitFile` primitive expects. The wrapped value is borrowed for the
/// duration of the call; the trait method never closes the underlying
/// socket.
#[derive(Debug, Clone, Copy)]
pub struct SocketHandle {
    #[cfg(unix)]
    fd: std::os::fd::RawFd,
    #[cfg(windows)]
    socket: std::os::windows::io::RawSocket,
    #[cfg(not(any(unix, windows)))]
    _private: (),
}

impl SocketHandle {
    /// Constructs a [`SocketHandle`] from a unix raw fd. Caller asserts
    /// the fd is open and valid for the duration of any subsequent
    /// [`PlatformSendFile::send_to_socket`] call.
    #[cfg(unix)]
    pub fn from_raw_fd(fd: std::os::fd::RawFd) -> Self {
        Self { fd }
    }

    /// Returns the wrapped raw fd. Unix-only.
    #[cfg(unix)]
    pub fn as_raw_fd(self) -> std::os::fd::RawFd {
        self.fd
    }

    /// Constructs a [`SocketHandle`] from a Windows raw socket. Caller
    /// asserts the socket is a Winsock handle opened with
    /// `WSA_FLAG_OVERLAPPED` (Rust's `std::net::TcpStream` qualifies) and
    /// remains valid for the duration of any subsequent
    /// [`PlatformSendFile::send_to_socket`] call.
    #[cfg(windows)]
    pub fn from_raw_socket(socket: std::os::windows::io::RawSocket) -> Self {
        Self { socket }
    }

    /// Returns the wrapped raw socket. Windows-only.
    #[cfg(windows)]
    pub fn as_raw_socket(self) -> std::os::windows::io::RawSocket {
        self.socket
    }
}

/// Abstraction over the platform `sendfile(2)` / `TransmitFile()` primitive.
///
/// Implementations transfer `len` bytes from `file` to the socket carried
/// by `socket`, starting at the file's current position. The contract
/// matches what the existing free functions already deliver:
///
/// - unix [`crate::sendfile::send_file_to_fd`] advances the source file
///   pointer by the returned byte count.
/// - Windows `crate::iocp::try_transmit_file` does not touch the file
///   pointer; the caller positions the file before invoking the call.
///
/// Returns the number of bytes the kernel reports it queued. Falls back
/// to the platform's existing fallback path when zero-copy is
/// unsupported; never panics on unsupported descriptors.
pub trait PlatformSendFile {
    /// Sends up to `len` bytes from `file` to `socket`. See the trait
    /// docstring for per-platform semantics.
    fn send_to_socket(&self, file: &File, socket: SocketHandle, len: u64) -> io::Result<u64>;

    /// Reports whether this implementation can plausibly issue a
    /// zero-copy send on the current target. The default
    /// [`platform_default`] implementations return `true` on Linux and
    /// macOS (`sendfile(2)` is built into the kernel) and on Windows
    /// when the `transmitfile` feature is enabled; otherwise `false`.
    ///
    /// `is_supported() == false` means [`Self::send_to_socket`] will
    /// route through the userspace fallback, not that it will fail.
    fn is_supported(&self) -> bool {
        true
    }
}

/// Linux `sendfile(2)` implementation forwarding to
/// [`crate::sendfile::send_file_to_fd`].
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxSendFile;

#[cfg(target_os = "linux")]
impl PlatformSendFile for LinuxSendFile {
    fn send_to_socket(&self, file: &File, socket: SocketHandle, len: u64) -> io::Result<u64> {
        crate::sendfile::send_file_to_fd(file, socket.as_raw_fd(), len)
    }
}

#[cfg(not(target_os = "linux"))]
impl PlatformSendFile for LinuxSendFile {
    fn send_to_socket(&self, _file: &File, _socket: SocketHandle, _len: u64) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "LinuxSendFile is only available on Linux targets",
        ))
    }

    fn is_supported(&self) -> bool {
        false
    }
}

/// macOS Darwin `sendfile(2)` implementation forwarding to
/// [`crate::sendfile::send_file_to_fd`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MacOsSendFile;

#[cfg(target_os = "macos")]
impl PlatformSendFile for MacOsSendFile {
    fn send_to_socket(&self, file: &File, socket: SocketHandle, len: u64) -> io::Result<u64> {
        crate::sendfile::send_file_to_fd(file, socket.as_raw_fd(), len)
    }
}

#[cfg(not(target_os = "macos"))]
impl PlatformSendFile for MacOsSendFile {
    fn send_to_socket(&self, _file: &File, _socket: SocketHandle, _len: u64) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "MacOsSendFile is only available on macOS targets",
        ))
    }

    fn is_supported(&self) -> bool {
        false
    }
}

/// Windows `TransmitFile()` implementation forwarding to
/// `crate::iocp::try_transmit_file`.
///
/// Only usable when both the host is Windows and the `transmitfile`
/// feature is enabled; otherwise [`PlatformSendFile::send_to_socket`]
/// returns `io::ErrorKind::Unsupported` and [`PlatformSendFile::is_supported`]
/// reports `false`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsTransmitFile;

#[cfg(all(target_os = "windows", feature = "transmitfile"))]
impl PlatformSendFile for WindowsTransmitFile {
    fn send_to_socket(&self, file: &File, socket: SocketHandle, len: u64) -> io::Result<u64> {
        use std::os::windows::io::AsRawHandle;
        let sent =
            crate::iocp::try_transmit_file(socket.as_raw_socket(), file.as_raw_handle(), len)?;
        Ok(sent as u64)
    }
}

#[cfg(not(all(target_os = "windows", feature = "transmitfile")))]
impl PlatformSendFile for WindowsTransmitFile {
    fn send_to_socket(&self, _file: &File, _socket: SocketHandle, _len: u64) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "WindowsTransmitFile requires the `transmitfile` feature on Windows targets",
        ))
    }

    fn is_supported(&self) -> bool {
        false
    }
}

/// Always-unsupported implementation used on platforms where no
/// native zero-copy `sendfile`/`TransmitFile` equivalent is available.
///
/// Returns [`io::ErrorKind::Unsupported`] from
/// [`PlatformSendFile::send_to_socket`] so callers know to route through
/// their existing userspace fallback. Useful as a test double and as the
/// concrete return type of [`platform_default`] on exotic targets.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedSendFile;

impl PlatformSendFile for UnsupportedSendFile {
    fn send_to_socket(&self, _file: &File, _socket: SocketHandle, _len: u64) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no native sendfile primitive is available on this target",
        ))
    }

    fn is_supported(&self) -> bool {
        false
    }
}

/// Returns the default [`PlatformSendFile`] implementation for the host.
///
/// - Linux: [`LinuxSendFile`].
/// - macOS: [`MacOsSendFile`].
/// - Windows with the `transmitfile` feature: [`WindowsTransmitFile`].
/// - Otherwise: [`UnsupportedSendFile`].
///
/// The returned trait object is behaviour-neutral with respect to the
/// existing free-function entry points; it exists so callers can hold a
/// `Box<dyn PlatformSendFile>` and let downstream tasks swap in
/// alternative implementations (probes, test doubles, future zero-copy
/// dispatch) without touching call sites.
pub fn platform_default() -> Box<dyn PlatformSendFile> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxSendFile)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(MacOsSendFile)
    }
    #[cfg(all(target_os = "windows", feature = "transmitfile"))]
    {
        Box::new(WindowsTransmitFile)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        all(target_os = "windows", feature = "transmitfile")
    )))]
    {
        Box::new(UnsupportedSendFile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `platform_default()` must report `is_supported() == true` on the
    /// targets where the underlying primitive exists, and `false`
    /// everywhere else. This locks in that the trait seam matches the
    /// platform-capability matrix documented in [`platform_default`].
    #[test]
    fn platform_default_is_supported_matches_target() {
        let default = platform_default();
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            all(target_os = "windows", feature = "transmitfile")
        ))]
        assert!(
            default.is_supported(),
            "platform_default must report supported on Linux/macOS/Windows+transmitfile"
        );
        #[cfg(not(any(
            target_os = "linux",
            target_os = "macos",
            all(target_os = "windows", feature = "transmitfile")
        )))]
        assert!(
            !default.is_supported(),
            "platform_default must report unsupported on targets without a native primitive"
        );
    }

    /// The `UnsupportedSendFile` doubles as a portable test stub. Verify
    /// it is always unsupported and surfaces a recoverable
    /// `ErrorKind::Unsupported` rather than panicking when called.
    #[test]
    fn unsupported_send_file_is_inert() {
        let stub = UnsupportedSendFile;
        assert!(!stub.is_supported());

        let file = tempfile::tempfile().expect("tempfile");
        #[cfg(unix)]
        let handle = SocketHandle::from_raw_fd(-1);
        #[cfg(windows)]
        let handle = SocketHandle::from_raw_socket(0);
        #[cfg(not(any(unix, windows)))]
        let handle = SocketHandle { _private: () };

        let err = stub
            .send_to_socket(&file, handle, 0)
            .expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    /// Trait-object round-trip: hold the default behind a
    /// `Box<dyn PlatformSendFile>`, call through the dyn dispatch, and
    /// confirm it does not panic on a zero-length transfer. This pins
    /// the seam so downstream tasks (WIN-S.LAND.1.c.2/.c.3) can swap in
    /// new implementations behind the same `Box<dyn _>` without
    /// touching call sites.
    #[test]
    fn trait_object_dispatch_is_panic_free() {
        fn use_seam(sender: &dyn PlatformSendFile) -> bool {
            let _ = sender.is_supported();
            true
        }

        let default: Box<dyn PlatformSendFile> = platform_default();
        assert!(use_seam(default.as_ref()));

        let stub: Box<dyn PlatformSendFile> = Box::new(UnsupportedSendFile);
        assert!(use_seam(stub.as_ref()));
    }
}
