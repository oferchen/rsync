//! `OC_RSYNC_PARALLEL_DELTA_SCAN` - the remote-shell server's switch for the
//! parallel delta scan.
//!
//! The client never forwards `--parallel-delta-scan`, because an upstream
//! server rejects unknown options, so on a pull the sending server decides for
//! itself. A remote-shell server reads this variable; a daemon reads its
//! `parallel delta scan` module parameter instead. Nothing goes on the wire.
//!
//! Grammar: `1`, `on`, `true`, `yes` (case-insensitive, surrounding whitespace
//! ignored) enable it. Unset, empty, or any other value keep the default (OFF).

/// Environment variable that enables the parallel delta scan on a
/// remote-shell server's sender.
pub(super) const PARALLEL_DELTA_SCAN_ENV: &str = "OC_RSYNC_PARALLEL_DELTA_SCAN";

/// Returns `true` when [`PARALLEL_DELTA_SCAN_ENV`] enables the parallel scan.
pub(super) fn parallel_delta_scan_enabled() -> bool {
    std::env::var(PARALLEL_DELTA_SCAN_ENV)
        .ok()
        .is_some_and(|value| enabled_from_value(&value))
}

/// Parses one raw environment value into the enable decision.
///
/// Split out from [`parallel_delta_scan_enabled`] so tests exercise the grammar
/// without mutating the process environment.
fn enabled_from_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes"
    )
}

#[cfg(test)]
mod tests {
    use super::enabled_from_value;

    #[test]
    fn enabling_values_are_recognized_case_insensitively() {
        for v in ["1", "on", "true", "yes", "ON", "True", " yes\n"] {
            assert!(enabled_from_value(v), "expected {v:?} to enable");
        }
    }

    // WHY: the scan stays opt-in, so anything that is not an explicit enable,
    // including typos, must keep the sequential scan.
    #[test]
    fn default_and_disabling_values_stay_off() {
        for v in ["", " ", "0", "off", "false", "no", "2", "enable", "yess"] {
            assert!(!enabled_from_value(v), "expected {v:?} to stay off");
        }
    }
}
