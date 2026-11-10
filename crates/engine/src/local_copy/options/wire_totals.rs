//! Wire-model totals for local copies.

use super::types::LocalCopyOptions;
use crate::local_copy::WireTotals;

impl LocalCopyOptions {
    /// Requests the wire-model `sent`/`received` totals for the run.
    ///
    /// Upstream prints a local copy's totals from the socketpair between its
    /// two forked halves (main.c:handle_stats). With `Some`, the run replays
    /// that stream into byte counters and reports the client's totals; with
    /// `None` (the default) no model is built and the totals count literal
    /// data only. Callers set it only when the totals will be shown, so a
    /// plain copy does no extra work.
    #[must_use]
    pub const fn wire_totals(mut self, totals: Option<WireTotals>) -> Self {
        self.wire_totals = totals;
        self
    }

    /// The wire-model parameters, when the totals were requested.
    #[must_use]
    pub const fn get_wire_totals(&self) -> Option<WireTotals> {
        self.wire_totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{CompatibilityFlags, ProtocolVersion};

    #[test]
    fn wire_totals_are_off_unless_requested() {
        assert!(LocalCopyOptions::new().get_wire_totals().is_none());
        let totals = WireTotals::new(ProtocolVersion::NEWEST, CompatibilityFlags::INC_RECURSE);
        assert_eq!(
            LocalCopyOptions::new()
                .wire_totals(Some(totals))
                .get_wire_totals(),
            Some(totals)
        );
    }
}
