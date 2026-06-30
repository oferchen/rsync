//! Receiver-side socket-read seam (NRX-1).
//!
//! The receiver ingest path reads wire bytes off a socket. Today that is a
//! blocking `read(2)` on a cloned `TcpStream` wrapped in `BufReader`. This
//! module introduces the abstraction the path reads *through* so a faster
//! backend (io_uring `RECV` on Linux, IOCP `WSARecv` on Windows) can be
//! selected without the caller naming the concrete reader.
//!
//! [`NetReader`](crate::net_reader::NetReader) is the abstraction;
//! [`for_socket`](crate::net_reader::for_socket) is the factory. The
//! factory delegates to the already-tested [`crate::socket_reader_from_fd`],
//! whose default arm is a behaviour-preserving standard blocking read, so
//! wiring this seam in is a no-op until an accelerated policy is selected.

use std::io::Read;

/// Receiver-side socket reader.
///
/// A marker trait composing [`Read`] + [`Send`] so the ingest path can hold a
/// `Box<dyn NetReader>` without depending on a concrete backend (io_uring
/// `RECV`, IOCP, or a plain blocking `read(2)`). This is the Dependency
/// Inversion seam: high-level receiver code depends on `NetReader`, not on any
/// one socket implementation.
///
/// The blanket impl means every existing `Read + Send` type - the standard
/// fallback reader and [`crate::IoUringSocketReader`] alike - satisfies the
/// trait with no extra code.
pub trait NetReader: Read + Send {}

impl<T: Read + Send> NetReader for T {}

/// Builds a receiver-side socket reader for `fd`.
///
/// Selects an io_uring `RECV` reader when the platform and `policy` permit and
/// the runtime probe succeeds; otherwise returns a standard buffered blocking
/// reader - byte-for-byte the path used before this seam existed. The default
/// arm makes adoption a behaviour-preserving no-op.
///
/// `fd` must be a valid socket file descriptor. Ownership is not transferred:
/// the caller must keep the owning socket alive for the lifetime of the
/// returned reader, exactly as [`crate::socket_reader_from_fd`] requires.
#[cfg(unix)]
pub fn for_socket(
    fd: std::os::unix::io::RawFd,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> std::io::Result<Box<dyn NetReader>> {
    Ok(Box::new(crate::socket_reader_from_fd(
        fd,
        buffer_capacity,
        policy,
    )?))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    /// The default (`Disabled`) arm reads the bytes written to the peer end and
    /// terminates at EOF - proving the std fallback path is wired correctly and
    /// that `for_socket` borrows rather than owns the fd.
    #[test]
    fn for_socket_reads_peer_bytes_via_default_arm() {
        let (mut writer, reader_sock) = UnixStream::pair().expect("socketpair");
        writer.write_all(b"hello wire").expect("write");
        drop(writer);

        let fd = reader_sock.as_raw_fd();
        let mut reader =
            for_socket(fd, 64 * 1024, crate::IoUringPolicy::Disabled).expect("for_socket");

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).expect("read_to_end");
        assert_eq!(buf, b"hello wire");

        // The owning socket must outlive the reader (fd is borrowed, not owned).
        drop(reader_sock);
    }

    /// The boxed result is usable through the trait object, confirming the
    /// Dependency Inversion seam: callers hold `dyn NetReader`, never a backend.
    #[test]
    fn for_socket_returns_dyn_net_reader() {
        let (mut writer, reader_sock) = UnixStream::pair().expect("socketpair");
        writer.write_all(b"x").expect("write");
        drop(writer);

        let fd = reader_sock.as_raw_fd();
        let boxed: Box<dyn NetReader> =
            for_socket(fd, 1024, crate::IoUringPolicy::Disabled).expect("for_socket");

        let mut boxed = boxed;
        let mut byte = [0u8; 1];
        boxed.read_exact(&mut byte).expect("read_exact");
        assert_eq!(&byte, b"x");
        drop(reader_sock);
    }
}
