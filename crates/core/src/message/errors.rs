use std::collections::TryReserveError;
use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
#[error("failed to reserve memory while rendering rsync message: {inner}")]
struct MessageBufferReserveError {
    #[source]
    inner: TryReserveError,
}

impl MessageBufferReserveError {
    #[inline]
    fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }
}

#[inline]
pub(super) fn map_message_reserve_error(err: TryReserveError) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        MessageBufferReserveError::new(err),
    )
}

#[cfg(test)]
mod tests {
    use super::{MessageBufferReserveError, map_message_reserve_error};
    use std::error::Error as _;

    #[test]
    fn mapped_error_wraps_original_try_reserve_error() {
        let mut buffer: Vec<u8> = Vec::new();
        let err = buffer.try_reserve(usize::MAX).unwrap_err();
        let io_error = map_message_reserve_error(err);

        assert_eq!(io_error.kind(), std::io::ErrorKind::OutOfMemory);
        assert!(io_error.to_string().contains("failed to reserve memory"));

        let source = io_error.source().expect("wrapped error");
        let try_reserve = source
            .downcast_ref::<std::collections::TryReserveError>()
            .expect("inner TryReserveError");
        assert!(try_reserve.to_string().contains("memory"));

        let message_error = io_error
            .get_ref()
            .and_then(|err| err.downcast_ref::<MessageBufferReserveError>())
            .expect("MessageBufferReserveError in chain");
        assert!(message_error.to_string().contains("rsync message"));
    }
}
