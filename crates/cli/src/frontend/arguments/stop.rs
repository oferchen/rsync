use std::ffi::OsString;
use std::time::SystemTime;

/// Distinguishes `--stop-after` (duration) from `--stop-at` (wall-clock time).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopRequestKind {
    /// Transfer should stop after a relative duration.
    StopAfter,
    /// Transfer should stop at an absolute wall-clock time.
    StopAt,
}

/// A parsed stop request with its computed deadline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StopRequest {
    kind: StopRequestKind,
    value: OsString,
    deadline: SystemTime,
}

impl StopRequest {
    /// Creates a `--stop-after` request with the given CLI value and deadline.
    pub(crate) const fn new_stop_after(value: OsString, deadline: SystemTime) -> Self {
        Self {
            kind: StopRequestKind::StopAfter,
            value,
            deadline,
        }
    }

    /// Creates a `--stop-at` request with the given CLI value and deadline.
    pub(crate) const fn new_stop_at(value: OsString, deadline: SystemTime) -> Self {
        Self {
            kind: StopRequestKind::StopAt,
            value,
            deadline,
        }
    }

    /// Returns whether this is a `--stop-after` or `--stop-at` request.
    #[allow(dead_code)]
    pub(crate) const fn kind(&self) -> StopRequestKind {
        self.kind
    }

    /// Returns the raw CLI value (e.g. `"60"` or `"12:00"`).
    #[allow(dead_code)]
    pub(crate) const fn cli_value(&self) -> &OsString {
        &self.value
    }

    /// Returns the computed absolute deadline as a `SystemTime`.
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
