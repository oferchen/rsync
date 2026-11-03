//! The handshake's reads, which upstream makes with `safe_read()`.

use std::fmt;
use std::io::{self, Read};

/// A failed read of the peer's stream before multiplexed I/O starts.
///
/// upstream: io.c:351-358 safe_read() - every read before io_start_*() goes
/// through it, and a failed `read()` is `rsyserr(FERROR, errno, "safe_read
/// failed to read %d bytes", len)` and `exit_cleanup(RERR_STREAMIO)`. `len` is
/// what the caller asked for, not what remained.
#[derive(Debug)]
pub struct SafeReadError {
    len: usize,
    source: io::Error,
}

impl SafeReadError {
    /// The byte count the failed read asked for.
    #[must_use]
    pub const fn requested(&self) -> usize {
        self.len
    }

    /// The error `read()` failed with.
    #[must_use]
    pub const fn source_error(&self) -> &io::Error {
        &self.source
    }

    /// Returns the handshake read failure carried by `error`, if it is one.
    #[must_use]
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }
}

impl fmt::Display for SafeReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "safe_read failed to read {} bytes: {}",
            self.len, self.source
        )
    }
}

impl std::error::Error for SafeReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Marks the failed reads of `inner` as [`SafeReadError`]s.
///
/// An end of stream stays as it is: upstream's safe_read() returns the short
/// count and the caller whines about EOF (io.c:2158-2161). So does a timeout,
/// which upstream's poll loop hands to check_timeout() (io.c:329-335).
pub(super) struct SafeRead<'a> {
    inner: &'a mut dyn Read,
}

impl<'a> SafeRead<'a> {
    pub(super) fn new(inner: &'a mut dyn Read) -> Self {
        Self { inner }
    }
}

fn mark(error: io::Error, len: usize) -> io::Error {
    match error.kind() {
        io::ErrorKind::UnexpectedEof | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => error,
        kind if SafeReadError::of(&error).is_none() => {
            io::Error::new(kind, SafeReadError { len, source: error })
        }
        _ => error,
    }
}

impl Read for SafeRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len();
        self.inner.read(buf).map_err(|error| mark(error, len))
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let len = buf.len();
        self.inner.read_exact(buf).map_err(|error| mark(error, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Failing(io::ErrorKind);

    impl Read for Failing {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }
    }

    #[test]
    fn a_failed_read_names_the_length_asked_for() {
        let mut inner = Failing(io::ErrorKind::ConnectionReset);
        let mut reader = SafeRead::new(&mut inner);
        let error = reader.read_exact(&mut [0u8; 4]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        let failed = SafeReadError::of(&error).expect("marked");
        assert_eq!(failed.requested(), 4);
        assert!(
            error
                .to_string()
                .starts_with("safe_read failed to read 4 bytes: ")
        );
    }

    #[test]
    fn an_end_of_stream_or_a_timeout_is_left_alone() {
        for kind in [io::ErrorKind::UnexpectedEof, io::ErrorKind::TimedOut] {
            let mut inner = Failing(kind);
            let mut reader = SafeRead::new(&mut inner);
            let error = reader.read_exact(&mut [0u8; 1]).unwrap_err();
            assert_eq!(error.kind(), kind);
            assert!(SafeReadError::of(&error).is_none(), "{kind:?}");
        }
    }
}
