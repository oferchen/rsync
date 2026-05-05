use std::ffi::OsString;
use std::time::SystemTime;

/// Distinguishes which user-facing flag produced a [`StopRequest`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopRequestKind {
    /// Produced by `--stop-after=DURATION`.
    StopAfter,
    /// Produced by `--stop-at=TIME`.
    StopAt,
}

/// Captures a parsed `--stop-after` or `--stop-at` request.
///
/// Holds the originating CLI text alongside the resolved absolute deadline so
/// the transfer can both honour the cut-off and forward the original argument
/// to remote peers verbatim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StopRequest {
    kind: StopRequestKind,
    value: OsString,
    deadline: SystemTime,
}

impl StopRequest {
    /// Builds a [`StopRequest`] for `--stop-after=DURATION`.
    pub(crate) const fn new_stop_after(value: OsString, deadline: SystemTime) -> Self {
        Self {
            kind: StopRequestKind::StopAfter,
            value,
            deadline,
        }
    }

    /// Builds a [`StopRequest`] for `--stop-at=TIME`.
    pub(crate) const fn new_stop_at(value: OsString, deadline: SystemTime) -> Self {
        Self {
            kind: StopRequestKind::StopAt,
            value,
            deadline,
        }
    }

    /// Returns which CLI flag produced this request.
    #[allow(dead_code)] // REASON: accessor used in unit tests
    pub(crate) const fn kind(&self) -> StopRequestKind {
        self.kind
    }

    /// Returns the original CLI text supplied by the user.
    #[allow(dead_code)] // REASON: accessor used in unit tests
    pub(crate) const fn cli_value(&self) -> &OsString {
        &self.value
    }

    /// Returns the absolute wall-clock deadline at which the transfer stops.
    pub(crate) const fn deadline(&self) -> SystemTime {
        self.deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stop_request_kind_eq() {
        assert_eq!(StopRequestKind::StopAfter, StopRequestKind::StopAfter);
        assert_eq!(StopRequestKind::StopAt, StopRequestKind::StopAt);
        assert_ne!(StopRequestKind::StopAfter, StopRequestKind::StopAt);
    }

    #[test]
    fn stop_request_new_stop_after() {
        let deadline = SystemTime::now() + Duration::from_secs(60);
        let request = StopRequest::new_stop_after(OsString::from("60"), deadline);

        assert_eq!(request.kind(), StopRequestKind::StopAfter);
        assert_eq!(request.cli_value(), &OsString::from("60"));
        assert_eq!(request.deadline(), deadline);
    }

    #[test]
    fn stop_request_new_stop_at() {
        let deadline = SystemTime::now();
        let request = StopRequest::new_stop_at(OsString::from("12:00"), deadline);

        assert_eq!(request.kind(), StopRequestKind::StopAt);
        assert_eq!(request.cli_value(), &OsString::from("12:00"));
        assert_eq!(request.deadline(), deadline);
    }

    #[test]
    fn stop_request_clone() {
        let deadline = SystemTime::now();
        let request = StopRequest::new_stop_after(OsString::from("30"), deadline);
        let cloned = request.clone();

        assert_eq!(request, cloned);
    }

    #[test]
    fn stop_request_debug() {
        let deadline = SystemTime::now();
        let request = StopRequest::new_stop_at(OsString::from("10:30"), deadline);

        let debug = format!("{request:?}");
        assert!(debug.contains("StopAt"));
        assert!(debug.contains("10:30"));
    }
}
