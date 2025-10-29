use std::collections::TryReserveError;
use std::fmt;
use std::io;

#[derive(Debug)]
pub(crate) struct LegacyLineReserveError {
    inner: TryReserveError,
}

impl LegacyLineReserveError {
    pub(crate) fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }
}

impl fmt::Display for LegacyLineReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to reserve memory for legacy negotiation buffer: {}",
            self.inner
        )
    }
}

impl std::error::Error for LegacyLineReserveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.inner)
    }
}

pub(crate) fn map_line_reserve_error_for_io(err: TryReserveError) -> io::Error {
    io::Error::new(io::ErrorKind::OutOfMemory, LegacyLineReserveError::new(err))
}
