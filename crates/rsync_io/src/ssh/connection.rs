use std::io::{self, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus};

/// Owns an active SSH subprocess and exposes its stdio handles.
pub struct SshConnection {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
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
            stdout,
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
}

impl Read for SshConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
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
