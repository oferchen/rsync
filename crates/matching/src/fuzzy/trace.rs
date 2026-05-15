//! `--debug=FUZZY` producer emissions for fuzzy basis matching.
//!
//! Mirrors upstream rsync's `generator.c` `DEBUG_GTE(FUZZY, N)` output so
//! wire-comparable diagnostics align across implementations.
//!
//! # Upstream Reference
//!
//! - `generator.c:1775` - `"fuzzy basis selected for %s: %s"` (level 1).
//! - `generator.c:847`  - `"fuzzy size/modtime match for %s"` (level 2).
//! - `generator.c:884`  - `"fuzzy distance for %s = %d.%05d"` (level 2).

use logging::debug_log;

/// Emits the level 1 announcement when a fuzzy basis is selected for a target.
///
/// Matches upstream rsync exactly:
/// ```text
/// fuzzy basis selected for path/to/target: path/to/basis
/// ```
///
/// upstream: generator.c:1775-1778.
#[inline]
pub fn trace_fuzzy_basis_selected(target: &str, basis: &str) {
    debug_log!(Fuzzy, 1, "fuzzy basis selected for {}: {}", target, basis);
}

/// Emits the level 2 size/modtime fast-path hit for a candidate.
///
/// Matches upstream rsync exactly:
/// ```text
/// fuzzy size/modtime match for path/to/candidate
/// ```
///
/// upstream: generator.c:847-848.
#[inline]
pub fn trace_fuzzy_size_mtime_match(candidate: &str) {
    debug_log!(Fuzzy, 2, "fuzzy size/modtime match for {}", candidate);
}

/// Emits the level 2 distance line for a scored candidate.
///
/// Upstream encodes `dist` as a 32-bit fixed-point value (high 16 bits =
/// integer part, low 16 bits = fractional part) and prints it as
/// `%d.%05d`. Our scoring function returns a single u32 score where higher
/// is better; we surface it under the upstream format string with the
/// integer slot occupied by the score and the fractional slot zeroed so
/// log parsers built for upstream output continue to work.
///
/// Matches upstream rsync format:
/// ```text
/// fuzzy distance for path/to/candidate = 42.00000
/// ```
///
/// upstream: generator.c:884-887.
#[inline]
pub fn trace_fuzzy_distance(candidate: &str, score: u32) {
    debug_log!(
        Fuzzy,
        2,
        "fuzzy distance for {} = {}.{:05}",
        candidate,
        score,
        0
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    /// Initializes the logger with the given FUZZY debug level and drains
    /// any preexisting events from prior tests.
    fn setup_fuzzy(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.fuzzy = level;
        init(cfg);
        let _ = drain_events();
    }

    /// Collects FUZZY debug messages emitted since the last drain.
    fn fuzzy_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Fuzzy,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// Pins the level 1 emission to upstream's format byte-for-byte.
    ///
    /// upstream: generator.c:1775-1778.
    #[test]
    fn fuzzy_basis_selected_matches_upstream() {
        setup_fuzzy(1);
        trace_fuzzy_basis_selected("dir/report_2024.csv", "dir/report_2023.csv");
        let msgs = fuzzy_messages();
        assert!(
            msgs.iter().any(
                |m| m == "fuzzy basis selected for dir/report_2024.csv: dir/report_2023.csv"
            ),
            "expected upstream-format FUZZY,1 emission, got {msgs:?}"
        );
    }

    /// Level 1 emissions must not fire when FUZZY debug is disabled.
    #[test]
    fn fuzzy_basis_selected_gated_by_level() {
        setup_fuzzy(0);
        trace_fuzzy_basis_selected("a", "b");
        assert!(fuzzy_messages().is_empty());
    }

    /// Pins the level 2 distance emission format.
    ///
    /// upstream: generator.c:884-887.
    #[test]
    fn fuzzy_distance_matches_upstream_format() {
        setup_fuzzy(2);
        trace_fuzzy_distance("dir/sibling.txt", 42);
        let msgs = fuzzy_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "fuzzy distance for dir/sibling.txt = 42.00000"),
            "expected upstream-format FUZZY,2 distance line, got {msgs:?}"
        );
    }

    /// Level 2 emissions must not fire when only FUZZY,1 is enabled.
    #[test]
    fn fuzzy_distance_gated_at_level_2() {
        setup_fuzzy(1);
        trace_fuzzy_distance("a", 1);
        assert!(fuzzy_messages().is_empty());
    }

    /// Pins the level 2 size/modtime fast-path emission format.
    ///
    /// upstream: generator.c:847-848.
    #[test]
    fn fuzzy_size_mtime_match_matches_upstream() {
        setup_fuzzy(2);
        trace_fuzzy_size_mtime_match("dir/cache.bin");
        let msgs = fuzzy_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "fuzzy size/modtime match for dir/cache.bin"),
            "expected upstream-format FUZZY,2 size/modtime line, got {msgs:?}"
        );
    }
}
