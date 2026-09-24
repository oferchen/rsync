//! `OC_RSYNC_LAZY_FLIST` - oc-internal staging flag for the lazy flist producer.
//!
//! The lazy incremental-recursion producer (upstream `flist.c:send_extra_file_list`
//! / `send1extra`) is built in stages behind this flag so each stage can prove wire
//! byte-neutrality before the next lands. See section 8 of
//! `docs/design/lazy-sender-inc-recurse.md`.
//!
//! The flag is oc-internal: it never changes a wire byte by itself, only which
//! producer fills the INC_RECURSE segments, so no capability or `-e` letter is
//! involved. Default OFF keeps the eager producer, byte-identical to upstream.
//!
//! Grammar: `1`, `on`, `true`, `yes` (case-insensitive, surrounding whitespace
//! ignored) enable it. Unset, empty, `0`, `off`, or any unrecognized value select
//! the default (OFF) - matching the project convention that every variable has a
//! tested default and invalid values apply it.

/// Environment variable that selects the lazy flist producer.
pub(crate) const LAZY_FLIST_ENV: &str = "OC_RSYNC_LAZY_FLIST";

/// Returns `true` when [`LAZY_FLIST_ENV`] enables the lazy flist producer.
///
/// Reads the environment on each call; callers invoke it once per transfer at the
/// INC_RECURSE partition seam, not per file. The default (unset or any value the
/// grammar does not recognize as enabling) is OFF, which keeps the eager producer
/// and a byte-identical wire.
pub(crate) fn lazy_flist_enabled() -> bool {
    std::env::var(LAZY_FLIST_ENV)
        .ok()
        .is_some_and(|value| enabled_from_value(&value))
}

/// Parses one raw environment value into the enable decision.
///
/// Split out from [`lazy_flist_enabled`] so tests exercise the value grammar
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
        for v in [
            "1", "on", "true", "yes", "ON", "True", "YES", " on ", "\tyes\n",
        ] {
            assert!(enabled_from_value(v), "expected {v:?} to enable");
        }
    }

    #[test]
    fn default_and_disabling_values_stay_off() {
        for v in [
            "", " ", "0", "off", "OFF", "false", "no", "2", "enable", "onoff", "yepp",
        ] {
            assert!(!enabled_from_value(v), "expected {v:?} to stay off");
        }
    }
}
