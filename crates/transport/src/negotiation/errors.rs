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

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    fn trigger_try_reserve_error() -> TryReserveError {
        Vec::<u8>::new()
            .try_reserve(usize::MAX)
            .err()
            .expect("overflow should produce a TryReserveError")
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
