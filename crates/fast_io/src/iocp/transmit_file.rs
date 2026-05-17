//! Windows `TransmitFile()` zero-copy file-to-socket primitive (#2301).
//!
//! `TransmitFile` lets the kernel DMA file bytes from the system file cache
//! straight to a TCP socket's send queue, eliminating the two user-space
//! copies that the read+`WSASend` loop performs (kernel page cache -> user
//! buffer, then user buffer -> socket send buffer). It is the Windows
//! counterpart to Linux `sendfile(2)` already exposed by
//! [`crate::sendfile::send_file_to_fd_with_policy`].
//!
//! # Scope
//!
//! This module exposes a single function, [`try_transmit_file`], that issues
//! a synchronous `TransmitFile` call against an already-overlapped socket.
//! The caller is responsible for:
//!
//! - opening the socket with `WSA_FLAG_OVERLAPPED` (Rust's `TcpStream` does
//!   this by default),
//! - having a regular-file `HANDLE` whose current file pointer is at the
//!   first byte to send,
//! - capping `length` so it fits in a `DWORD` (the per-call ceiling is
//!   `u32::MAX`; callers that need more loop and bump the file pointer),
//! - falling back to [`crate::iocp::socket::IocpSocketWriter::send_async`]
//!   when this function returns `io::ErrorKind::Unsupported`
//!   (`ERROR_NOT_SUPPORTED`), which Windows returns on SMB, DFS, encrypted
//!   volumes, and other unusual filesystems.
//!
//! # Synchronous completion
//!
//! Passing a null `OVERLAPPED` makes Windows perform the transmit
//! synchronously, blocking the worker thread until the kernel has copied
//! the entire range into the socket buffer. The function returns the number
//! of bytes the kernel reports it queued. The asynchronous variant
//! (`OVERLAPPED` + IOCP completion) is tracked separately under the
//! design's step 3 in
//! `docs/design/windows-transmitfile-zerocopy.md`; this primitive lands
//! the synchronous fast path first because the daemon TCP send loop runs
//! on a dedicated worker that can absorb the block.
//!
//! # Header / trailer iovecs
//!
//! Multiplex framing (`MSG_DATA` envelope) is intentionally not handled
//! here: the daemon path attaches headers through the multiplex writer
//! before this primitive ever runs. `lpTransmitBuffers` is therefore
//! passed as null. Adding header support is a follow-up tracked in the
//! same design doc.

#![cfg(all(target_os = "windows", feature = "transmitfile"))]

use std::io;
use std::os::windows::io::{RawHandle, RawSocket};

use windows_sys::Win32::Foundation::{ERROR_NOT_SUPPORTED, HANDLE};
use windows_sys::Win32::Networking::WinSock::{SOCKET, TransmitFile};

/// Upper bound on a single `TransmitFile()` call: `nNumberOfBytesToWrite` is
/// a `DWORD` (32-bit unsigned). Callers loop and advance the file pointer
/// when they have more than `u32::MAX` bytes to send.
pub const TRANSMIT_FILE_MAX_BYTES: u64 = u32::MAX as u64;

/// `nNumberOfBytesPerSend` per the design's section 1: 64 KiB matches
/// upstream rsync's `IO_BUFFER_SIZE` and avoids the driver's 1-MSS cap on
/// NICs without TSO when this argument is left at zero.
const BYTES_PER_SEND: u32 = 64 * 1024;

/// Attempts a synchronous zero-copy `TransmitFile()` from `file` to `socket`.
///
/// On success returns the number of bytes the kernel reports it queued
/// onto the socket (always equal to `length` for synchronous completion
/// because the call is all-or-nothing). On failure returns an
/// [`io::Error`] whose `kind()` is one of:
///
/// - [`io::ErrorKind::Unsupported`] when Windows returns
///   `ERROR_NOT_SUPPORTED` (SMB/DFS/encrypted volume, or the socket is
///   not a TCP socket, or the handle is not a regular file). The caller
///   must fall back to a regular `WSASend` loop.
/// - [`io::ErrorKind::InvalidInput`] when `length` exceeds
///   [`TRANSMIT_FILE_MAX_BYTES`]. Callers must chunk and loop.
/// - Any other variant for genuine I/O errors (`BrokenPipe`,
///   `ConnectionReset`, ...).
///
/// # Arguments
///
/// - `socket`: raw Winsock handle. Must be a TCP socket opened with
///   `WSA_FLAG_OVERLAPPED`. Rust's `std::net::TcpStream` satisfies both.
/// - `file`: raw Win32 file handle. Must be opened with `GENERIC_READ`.
///   The current file pointer determines the starting offset; callers
///   that want a specific offset call `SetFilePointerEx` first.
/// - `length`: number of bytes to transmit. Must not exceed
///   [`TRANSMIT_FILE_MAX_BYTES`].
///
/// # Safety boundary
///
/// This is a safe public API. The single `unsafe` block inside wraps the
/// FFI call: both handles are caller-supplied and required to be valid
/// for the duration of the call; no pointers are exposed back to the
/// caller; the OVERLAPPED argument is null, so the kernel does not
/// retain any borrowed memory after return.
pub fn try_transmit_file(socket: RawSocket, file: RawHandle, length: u64) -> io::Result<usize> {
    if length == 0 {
        return Ok(0);
    }
    if length > TRANSMIT_FILE_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "TransmitFile length {length} exceeds DWORD cap {TRANSMIT_FILE_MAX_BYTES}; caller must chunk"
            ),
        ));
    }

    let bytes = length as u32;

    // SAFETY: `socket` is a Winsock handle the caller pinky-promised was
    // created with WSA_FLAG_OVERLAPPED. `file` is a regular-file HANDLE
    // open for reading. `lpOverlapped` and `lpTransmitBuffers` are both
    // null, so the kernel does not retain any pointer from this stack
    // frame past the call. The TransmitFile signature itself takes the
    // socket and file by value (`SOCKET`/`HANDLE` are integer-sized
    // handles), so there is no aliasing of Rust memory to worry about.
    #[allow(unsafe_code)]
    let ok = unsafe {
        TransmitFile(
            socket as SOCKET,
            file as HANDLE,
            bytes,
            BYTES_PER_SEND,
            std::ptr::null_mut(),
            std::ptr::null(),
            0,
        )
    };

    if ok != 0 {
        // Synchronous TransmitFile is all-or-nothing: it returned TRUE,
        // so the kernel queued the full requested length.
        return Ok(bytes as usize);
    }

    Err(map_transmit_file_error(io::Error::last_os_error()))
}

/// Translates a `TransmitFile()` failure into an [`io::Error`] whose
/// `kind()` lets callers branch on `Unsupported` without inspecting raw
/// Win32 codes.
fn map_transmit_file_error(err: io::Error) -> io::Error {
    if let Some(code) = err.raw_os_error()
        && code == ERROR_NOT_SUPPORTED as i32
    {
        return io::Error::new(io::ErrorKind::Unsupported, err);
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::windows::io::{AsRawHandle, AsRawSocket};
    use std::thread;
    use tempfile::NamedTempFile;

    /// Round-trips a 64 KiB file across a localhost TCP pair via
    /// `try_transmit_file` and byte-compares the result.
    #[test]
    fn transmit_file_roundtrip_64k() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let payload: Vec<u8> = (0..65_536u32).map(|i| (i as u8).wrapping_mul(31)).collect();

        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&payload).unwrap();
        tmp.flush().unwrap();

        let expected = payload.clone();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut received = Vec::with_capacity(expected.len());
            sock.read_to_end(&mut received).unwrap();
            received
        });

        let client = TcpStream::connect(addr).unwrap();

        // Re-open the temp file in read mode so the file pointer starts
        // at byte 0 regardless of the write the test just performed.
        let file = OpenOptions::new().read(true).open(tmp.path()).unwrap();

        let sent = try_transmit_file(
            client.as_raw_socket(),
            file.as_raw_handle(),
            payload.len() as u64,
        )
        .expect("TransmitFile must succeed on local NTFS / ReFS");

        assert_eq!(sent, payload.len());

        // Signal EOF to the server thread so its `read_to_end` returns.
        drop(client);
        drop(file);

        let received = server.join().unwrap();
        assert_eq!(received, payload, "byte-for-byte round-trip");
    }

    /// Length larger than `DWORD` is rejected before any FFI is issued.
    #[test]
    fn transmit_file_rejects_oversized_length() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let _ = listener.accept().unwrap();
        });

        let client = TcpStream::connect(addr).unwrap();
        let tmp = NamedTempFile::new().unwrap();
        let file = OpenOptions::new().read(true).open(tmp.path()).unwrap();

        let err = try_transmit_file(
            client.as_raw_socket(),
            file.as_raw_handle(),
            TRANSMIT_FILE_MAX_BYTES + 1,
        )
        .expect_err("oversized length must be refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        drop(client);
        server.join().unwrap();
    }

    /// Zero-length transmit short-circuits without touching the kernel.
    #[test]
    fn transmit_file_zero_length_is_noop() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let _ = listener.accept().unwrap();
        });

        let client = TcpStream::connect(addr).unwrap();
        let tmp = NamedTempFile::new().unwrap();
        let file = OpenOptions::new().read(true).open(tmp.path()).unwrap();

        let sent = try_transmit_file(client.as_raw_socket(), file.as_raw_handle(), 0).unwrap();
        assert_eq!(sent, 0);

        drop(client);
        server.join().unwrap();
    }

    /// Calling against a non-socket handle (a regular file) surfaces as a
    /// recoverable error, not a panic. Windows reports
    /// `WSAENOTSOCK`; this test only asserts that we surface *some* error.
    #[test]
    fn transmit_file_non_socket_target_returns_error() {
        let src = NamedTempFile::new().unwrap();
        let src_file = OpenOptions::new().read(true).open(src.path()).unwrap();

        // Use a second regular file's handle in place of a SOCKET. The
        // cast is well-formed at the C-ABI level (both are pointer-sized),
        // and Windows is expected to reject the call.
        let dst = NamedTempFile::new().unwrap();
        let dst_file = OpenOptions::new().write(true).open(dst.path()).unwrap();

        let fake_socket = dst_file.as_raw_handle() as RawSocket;
        let result = try_transmit_file(fake_socket, src_file.as_raw_handle(), 16);
        assert!(result.is_err(), "non-socket destination must fail");
    }
}
