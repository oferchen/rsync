//! SSH connection management with split read/write support.

#![allow(unsafe_code)]

use std::io::{self, Read, Write};
use std::mem::ManuallyDrop;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus};

/// Owns an active SSH subprocess and exposes its stdio handles.
pub struct SshConnection {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

impl SshConnection {
    /// Constructs a new connection from the spawned child process.
    pub(super) const fn new(
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: ChildStdout,
        stderr: Option<ChildStderr>,
    ) -> Self {
        Self {
            child,
            stdin,
            stdout: Some(stdout),
            stderr,
        }
    }

    /// Returns a mutable reference to the subprocess stderr stream, when available.
    pub const fn stderr_mut(&mut self) -> Option<&mut ChildStderr> {
        self.stderr.as_mut()
    }

    /// Transfers ownership of the subprocess stderr stream to the caller.
    ///
    /// This helper complements [`stderr_mut`](Self::stderr_mut) by allowing
    /// higher layers to move the stderr handle into background readers without
    /// keeping the connection borrowed mutably for the lifetime of the stream.
    /// Subsequent calls return `None`, matching the semantics of
    /// [`Option::take`].
    #[must_use]
    pub const fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    /// Flushes and closes the stdin pipe, signalling EOF to the subprocess.
    pub fn close_stdin(&mut self) -> io::Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin.flush()?;
        }
        Ok(())
    }

    /// Waits for the subprocess to exit, consuming the connection.
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        let _ = self.close_stdin();
        self.child.wait()
    }

    /// Attempts to retrieve the subprocess exit status without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Splits the connection into separate read and write halves for bidirectional I/O.
    ///
    /// This consumes the connection and returns:
    /// - A reader (stdout) for receiving data from the remote process
    /// - A writer (stdin) for sending data to the remote process
    /// - An owned handle for waiting on the child process
    ///
    /// # Returns
    ///
    /// Returns `(reader, writer, child_handle)` on success.
    /// Returns an error if stdin or stdout has already been taken.
    pub fn split(self) -> io::Result<(SshReader, SshWriter, SshChildHandle)> {
        // Use ManuallyDrop to prevent Drop from running - we're moving fields out
        let mut this = ManuallyDrop::new(self);

        let stdin = this.stdin.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "stdin has already been closed")
        })?;

        let stdout = this.stdout.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "stdout has already been taken")
        })?;

        // SAFETY: We've wrapped self in ManuallyDrop, so these moves are safe.
        // We take ownership of child and stderr using ptr::read since we can't
        // move out of a ManuallyDrop directly.
        let child = unsafe { std::ptr::read(&this.child) };
        let stderr = this.stderr.take();

        Ok((
            SshReader { stdout },
            SshWriter { stdin },
            SshChildHandle {
                child,
                _stderr: stderr,
            },
        ))
    }
}

/// Read half of an SSH connection (subprocess stdout).
pub struct SshReader {
    stdout: ChildStdout,
}

impl Read for SshReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

/// Write half of an SSH connection (subprocess stdin).
pub struct SshWriter {
    stdin: ChildStdin,
}

impl Write for SshWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

impl SshWriter {
    /// Flushes and closes the stdin pipe, signalling EOF to the subprocess.
    pub fn close(mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

/// Handle to wait for SSH subprocess completion.
pub struct SshChildHandle {
    child: Child,
    _stderr: Option<ChildStderr>,
}

impl SshChildHandle {
    /// Waits for the subprocess to exit.
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }
}

impl Read for SshConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stdout.as_mut() {
            Some(stdout) => stdout.read(buf),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdout has already been taken",
            )),
        }
    }
}

impl Write for SshConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.write(buf),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdin has already been closed",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for SshConnection {
    fn drop(&mut self) {
        let _ = self.close_stdin();

        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }

        let _ = self.child.wait();
    }
}
