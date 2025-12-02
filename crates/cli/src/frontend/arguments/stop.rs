use std::ffi::OsString;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopRequestKind {
    StopAfter,
    StopAt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StopRequest {
    kind: StopRequestKind,
    value: OsString,
    deadline: SystemTime,
}

impl StopRequest {
    pub(crate) fn new_stop_after(value: OsString, deadline: SystemTime) -> Self {
        Self {
            kind: StopRequestKind::StopAfter,
            value,
            deadline,
        }
    }

    pub(crate) fn new_stop_at(value: OsString, deadline: SystemTime) -> Self {
        Self {
            kind: StopRequestKind::StopAt,
            value,
            deadline,
        }
    }

    /// Returns the kind of stop request
    #[allow(dead_code)]
    pub(crate) const fn kind(&self) -> StopRequestKind {
        self.kind
    }

    /// Returns the original CLI value
    #[allow(dead_code)]
    pub(crate) fn cli_value(&self) -> &OsString {
        &self.value
    }

    pub(crate) const fn deadline(&self) -> SystemTime {
        self.deadline
    }
}
