//! # Overview
//!
//! Cross-platform connection primitive that yields a pair of byte streams
//! used as the stderr aux-channel between the parent process and a spawned
//! `ssh` child. Created for task SSE-3 (#2372) per
//! `docs/design/socketpair-stderr-channel.md`. SSE-4 layers the async
//! drain on top of this primitive.
//!
//! # Design
//!
//! - **Unix**: `socketpair(AF_UNIX, SOCK_STREAM, 0)` via
//!   `std::os::unix::net::UnixStream::pair()`. The stdlib path requests
//!   `SOCK_CLOEXEC` automatically, so both ends are close-on-exec by
//!   default and do not need manual `fcntl(F_SETFD, FD_CLOEXEC)`. Both
//!   halves are converted into [`std::fs::File`] through the safe
//!   `OwnedFd` bridge so callers can store both ends behind the same
//!   handle type as the pipe fallback.
//! - **Windows**: `socketpair(2)` does not exist on Win32. The
//!   behaviourally-equivalent shim (design doc Section 3.2) binds a
//!   `TcpListener` to `127.0.0.1:0`, connects back, accepts, and drops
//!   the listener. Wiring that result into a `File`-typed parent-side
//!   handle requires the `OwnedHandle`/`RawSocket` bridge implemented
//!   in the `fast_io` crate, which `rsync_io` must not duplicate
//!   (workspace `#![deny(unsafe_code)]` policy). SSE-5 (#2374) lands
//!   the Windows shim in coordination with `fast_io`; until then the
//!   Windows implementation here returns
//!   `io::ErrorKind::Unsupported` so the caller falls back to
//!   `Stdio::piped()` exactly like the Unix FD-exhaustion path does
//!   in `aux_channel.rs::configure_stderr_channel`.
//!
//! # Invariants
//!
//! - Both halves are returned in a connected state. Writes on one half
//!   become readable on the other half.
//! - Closing one half causes the peer to observe EOF (`read` returns 0).
//! - On Unix both halves carry `FD_CLOEXEC` (set by the stdlib).
//!
//! # Errors
//!
//! All constructors return [`std::io::Error`]. Callers should treat
//! every error as a signal to fall back to `Stdio::piped()`, mirroring
//! the existing fallback in `aux_channel.rs::configure_stderr_channel`.
//!
//! # Examples
//!
//! ```ignore
//! use rsync_io::ssh::socketpair_stderr::make_stderr_socketpair;
//!
//! let (parent, child) = make_stderr_socketpair()?;
//! // `child` is handed to the spawned subprocess as fd 2; `parent` is
//! // drained on this side.
//! # Ok::<(), std::io::Error>(())
//! ```

use std::fs::File;
use std::io;

#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// Creates a connected pair of byte streams suitable for use as the
/// stderr aux-channel between a parent process and a spawned `ssh`
/// child.
///
/// On Unix the result is the parent/child halves of a
/// `socketpair(AF_UNIX, SOCK_STREAM, 0)`. On Windows the call returns
/// `io::ErrorKind::Unsupported` until SSE-5 lands the loopback shim
/// alongside the safe handle wrapper in `fast_io`.
///
/// Returning [`File`] on both halves lets the caller store the handles
/// uniformly with the existing pipe-backed fallback path.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when the kernel refuses to
/// create the pair. On Windows always returns
/// `io::ErrorKind::Unsupported`.
pub fn make_stderr_socketpair() -> io::Result<(File, File)> {
    make_stderr_socketpair_impl()
}

#[cfg(unix)]
fn make_stderr_socketpair_impl() -> io::Result<(File, File)> {
    let (parent, child) = UnixStream::pair()?;
    let parent_fd: OwnedFd = parent.into();
    let child_fd: OwnedFd = child.into();
    Ok((File::from(parent_fd), File::from(child_fd)))
}

#[cfg(not(unix))]
fn make_stderr_socketpair_impl() -> io::Result<(File, File)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "stderr socketpair primitive is not yet available on this platform (tracked under SSE-5)",
    ))
}

/// Marks the given handle as non-blocking on Unix. On non-Unix
/// targets this is a no-op so callers can keep a single code path.
///
/// Provided as a helper because the SSE-4 async drain will need
/// non-blocking semantics on the parent half before registering it
/// with the tokio reactor. The flag is stored on the open-file
/// description, so a duplicated `UnixStream` view of the same fd
/// suffices to flip the mode for every alias.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when the platform refuses the
/// mode change.
pub fn set_nonblocking(handle: &File, nonblocking: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;

        // `BorrowedFd::try_clone_to_owned` issues `fcntl(F_DUPFD_CLOEXEC)`
        // under the hood and is entirely safe stdlib API. The cloned
        // descriptor shares the open-file description with the
        // original, so `set_nonblocking` toggles the `O_NONBLOCK`
        // flag visible to both fds.
        let dup_fd: OwnedFd = handle.as_fd().try_clone_to_owned()?;
        let stream = UnixStream::from(dup_fd);
        stream.set_nonblocking(nonblocking)?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = (handle, nonblocking);
        Ok(())
    }
}

#[cfg(all(test, unix, feature = "ssh-socketpair-stderr"))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Round-trips a 1 KiB payload through the socketpair to confirm
    /// both halves are wired together correctly.
    #[test]
    fn round_trip_one_kilobyte() {
        let (mut parent, mut child) = make_stderr_socketpair().expect("create socketpair");

        let payload: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        child.write_all(&payload).expect("write payload");
        child.flush().expect("flush child");

        let mut received = vec![0u8; payload.len()];
        parent.read_exact(&mut received).expect("read payload");
        assert_eq!(received, payload);
    }

    /// Dropping one half must surface `read == 0` on the peer.
    #[test]
    fn closed_peer_reports_eof() {
        let (mut parent, child) = make_stderr_socketpair().expect("create socketpair");
        drop(child);

        let mut buf = [0u8; 64];
        let n = parent.read(&mut buf).expect("read after peer close");
        assert_eq!(n, 0, "peer close must surface as EOF");
    }

    /// `set_nonblocking` must succeed on the parent end and the mode
    /// flip must persist for subsequent reads.
    #[test]
    fn set_nonblocking_toggles_mode() {
        let (parent, _child) = make_stderr_socketpair().expect("create socketpair");
        set_nonblocking(&parent, true).expect("enable non-blocking");

        let mut buf = [0u8; 8];
        // With no data and non-blocking mode, the read should fail
        // with `WouldBlock` rather than blocking forever.
        let err = (&parent)
            .read(&mut buf)
            .expect_err("non-blocking read with no data must fail");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        set_nonblocking(&parent, false).expect("restore blocking");
    }
}
