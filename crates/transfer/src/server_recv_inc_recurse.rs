//! `OC_RSYNC_TEST_SERVER_RECV_INC_RECURSE` - test-only switch that lets a
//! server receiver negotiate incremental recursion.
//!
//! Upstream's server sets `CF_INC_RECURSE` whenever `set_allow_inc_recurse()`
//! allows it and the client sent `'i'`, whatever the transfer direction
//! (`compat.c:162-181`, `compat.c:745-746`). oc-rsync's server receiver still
//! withholds it (see `compute_allow_inc_recurse`), so every push into an oc
//! server runs without INC_RECURSE. This switch lifts that role restriction
//! so the testsuite and the e2e tests can drive the server receiver's
//! incremental path before the production gate opens.
//!
//! Default OFF keeps the push wire byte-identical to earlier releases.
//!
//! Grammar: `1`, `on`, `true`, `yes` (case-insensitive, surrounding whitespace
//! ignored) enable it. Unset, empty, `0`, `off`, or any unrecognized value
//! select the default (OFF).

/// Environment variable that lets a server receiver negotiate INC_RECURSE.
pub(crate) const SERVER_RECV_INC_RECURSE_ENV: &str = "OC_RSYNC_TEST_SERVER_RECV_INC_RECURSE";

/// Returns `true` when [`SERVER_RECV_INC_RECURSE_ENV`] enables INC_RECURSE on
/// the server receiver.
pub(crate) fn server_recv_inc_recurse_enabled() -> bool {
    std::env::var(SERVER_RECV_INC_RECURSE_ENV)
        .ok()
        .is_some_and(|value| enabled_from_value(&value))
}

/// Parses one raw environment value into the enable decision.
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

    #[test]
    fn other_values_leave_the_switch_off() {
        for v in ["", "0", "off", "false", "no", "2", "enable"] {
            assert!(!enabled_from_value(v), "expected {v:?} to leave it off");
        }
    }
}
