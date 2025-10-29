use std::collections::TryReserveError;
use std::fmt;
use std::io;

#[derive(Debug)]
struct MessageBufferReserveError {
    inner: TryReserveError,
}

impl MessageBufferReserveError {
    #[inline]
    fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }

    #[inline]
    fn inner(&self) -> &TryReserveError {
        &self.inner
    }
}

impl fmt::Display for MessageBufferReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to reserve memory while rendering rsync message: {}",
            self.inner
        )
    }
}

impl std::error::Error for MessageBufferReserveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.inner())
    }
}

#[inline]
pub(super) fn map_message_reserve_error(err: TryReserveError) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        MessageBufferReserveError::new(err),
    )
}
