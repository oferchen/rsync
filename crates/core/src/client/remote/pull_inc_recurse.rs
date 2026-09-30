//! `OC_RSYNC_PULL_INC_RECURSE` - staging flag for incremental recursion on pulls.
//!
//! Upstream's client advertises the `'i'` capability whenever
//! `set_allow_inc_recurse()` leaves `allow_inc_recurse` set, whatever the
//! transfer direction (`compat.c:162-181`, `options.c:3049`). oc-rsync's client
//! has so far withheld it whenever it is the receiver, so every pull ran
//! without INC_RECURSE. This flag stages the receiver's lazy sub-list
//! consumption: when it is set, a pulling client advertises `'i'` under
//! upstream's own conditions.
//!
//! One oc-only restriction remains while the flag is staged: `'i'` stays
//! withheld whenever a delete pass would run. The receiver's per-directory
//! delete over a partially received list is not built yet, and a whole-list
//! sweep over an incomplete list would classify every not-yet-received entry
//! as extraneous.
//!
//! Default OFF keeps the pull wire byte-identical to earlier releases.
//!
//! Grammar: `1`, `on`, `true`, `yes` (case-insensitive, surrounding whitespace
//! ignored) enable it. Unset, empty, `0`, `off`, or any unrecognized value
//! select the default (OFF).

use crate::client::config::ClientConfig;

/// Environment variable that lets a pulling client advertise INC_RECURSE.
pub(crate) const PULL_INC_RECURSE_ENV: &str = "OC_RSYNC_PULL_INC_RECURSE";

/// Returns `true` when [`PULL_INC_RECURSE_ENV`] enables INC_RECURSE on pulls.
///
/// Read once per invocation when the server argument string is built.
fn pull_inc_recurse_enabled() -> bool {
    std::env::var(PULL_INC_RECURSE_ENV)
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

/// Decides whether the client advertises the `'i'` (INC_RECURSE) capability.
///
/// `am_sender` is the local side's role: `true` on a push, `false` on a pull.
///
/// # Upstream Reference
///
/// - `compat.c:162-181 set_allow_inc_recurse()` - the option gate, resolved by
///   [`ClientConfig::allow_inc_recurse`].
/// - `options.c:3049 maybe_add_e_option()` - `if (allow_inc_recurse)` adds `'i'`.
pub(crate) fn advertise_inc_recurse(config: &ClientConfig, am_sender: bool) -> bool {
    advertise_inc_recurse_with(config, am_sender, pull_inc_recurse_enabled())
}

/// [`advertise_inc_recurse`] with the staging flag passed in, so tests need not
/// mutate the process environment.
fn advertise_inc_recurse_with(config: &ClientConfig, am_sender: bool, pull_opt_in: bool) -> bool {
    if !config.allow_inc_recurse(am_sender) {
        return false;
    }
    am_sender || (pull_opt_in && !(config.delete() || config.delete_excluded()))
}

#[cfg(test)]
mod tests {
    use super::{advertise_inc_recurse_with, enabled_from_value};
    use crate::client::config::ClientConfig;

    fn recursive() -> ClientConfig {
        ClientConfig::builder().recursive(true).build()
    }

    #[test]
    fn enabling_values_are_recognized_case_insensitively() {
        for v in ["1", "on", "true", "yes", "ON", "True", " yes\n"] {
            assert!(enabled_from_value(v), "expected {v:?} to enable");
        }
    }

    #[test]
    fn default_and_disabling_values_stay_off() {
        for v in ["", "0", "off", "false", "no", "2", "enable"] {
            assert!(!enabled_from_value(v), "expected {v:?} to stay off");
        }
    }

    #[test]
    fn push_advertises_regardless_of_the_pull_flag() {
        let config = recursive();
        assert!(advertise_inc_recurse_with(&config, true, false));
        assert!(advertise_inc_recurse_with(&config, true, true));
    }

    #[test]
    fn pull_advertises_only_when_opted_in() {
        // Default off: a pull keeps the historical no-inc-recurse wire.
        let config = recursive();
        assert!(!advertise_inc_recurse_with(&config, false, false));
        assert!(advertise_inc_recurse_with(&config, false, true));
    }

    #[test]
    fn pull_opt_in_still_honours_the_upstream_option_gate() {
        // upstream: compat.c:172 - `!recurse || use_qsort` clears it, and
        // --no-inc-recursive clears it outright.
        let flat = ClientConfig::builder().recursive(false).build();
        assert!(!advertise_inc_recurse_with(&flat, false, true));
        let qsort = ClientConfig::builder().recursive(true).qsort(true).build();
        assert!(!advertise_inc_recurse_with(&qsort, false, true));
        let no_ir = ClientConfig::builder()
            .recursive(true)
            .inc_recursive_send(false)
            .build();
        assert!(!advertise_inc_recurse_with(&no_ir, false, true));
    }

    #[test]
    fn pull_opt_in_withholds_i_whenever_a_delete_pass_would_run() {
        // A delete sweep over a partially received list would unlink every
        // entry not yet received, so no delete mode may pull incrementally
        // until the per-directory delete lands.
        let base = || ClientConfig::builder().recursive(true);
        let cases: [(&str, ClientConfig); 5] = [
            ("--delete", base().delete(true).build()),
            ("--delete-during", base().delete_during().build()),
            ("--delete-delay", base().delete_delay(true).build()),
            ("--delete-before", base().delete_before(true).build()),
            ("--delete-after", base().delete_after(true).build()),
        ];
        for (name, config) in cases {
            assert!(
                !advertise_inc_recurse_with(&config, false, true),
                "{name} must withhold 'i' on a pull"
            );
            assert!(
                advertise_inc_recurse_with(&config, true, true),
                "{name} on a push is the remote receiver's decision"
            );
        }
        let excluded = ClientConfig::builder()
            .recursive(true)
            .delete_excluded(true)
            .build();
        assert!(!advertise_inc_recurse_with(&excluded, false, true));
    }
}
