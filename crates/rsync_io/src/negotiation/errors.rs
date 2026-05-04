use std::collections::TryReserveError;
use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
#[error("failed to reserve memory for legacy negotiation buffer: {inner}")]
pub(crate) struct LegacyLineReserveError {
    #[source]
    inner: TryReserveError,
}

impl LegacyLineReserveError {
    pub(crate) const fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }
}

pub(crate) fn map_line_reserve_error_for_io(err: TryReserveError) -> io::Error {
    io::Error::new(io::ErrorKind::OutOfMemory, LegacyLineReserveError::new(err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    fn trigger_try_reserve_error() -> TryReserveError {
        Vec::<u8>::new()
            .try_reserve(usize::MAX)
            .expect_err("overflow should produce a TryReserveError")
    }

    #[test]
    fn legacy_line_reserve_error_reports_source_and_message() {
        let inner = trigger_try_reserve_error();
        let error = LegacyLineReserveError::new(inner);

        let description = error.to_string();
        assert!(
            description.contains("failed to reserve memory for legacy negotiation buffer"),
            "error description should describe the negotiation buffer failure"
        );
        assert!(
            description.contains("capacity overflow") || description.contains("memory"),
            "error description should include the underlying allocator failure"
        );

        assert!(
            error.source().is_some(),
            "wrapper error should expose the original TryReserveError as its source"
        );
    }

    #[test]
    fn map_line_reserve_error_wraps_try_reserve_error_for_io() {
        let io_error = map_line_reserve_error_for_io(trigger_try_reserve_error());

        assert_eq!(io_error.kind(), io::ErrorKind::OutOfMemory);

        let message = io_error.to_string();
        assert!(
            message.contains("failed to reserve memory for legacy negotiation buffer"),
            "IO error should mention the negotiation buffer failure"
        );

        let wrapper = io_error
            .get_ref()
            .and_then(|source| source.downcast_ref::<LegacyLineReserveError>())
            .expect("IO error should wrap a LegacyLineReserveError");
        assert!(
            wrapper.source().is_some(),
            "wrapped error should retain access to the original TryReserveError"
        );
    }
}
