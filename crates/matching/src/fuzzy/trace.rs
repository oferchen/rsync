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
/// Upstream encodes `dist` as a 32-bit fixed-point value: the high 16 bits are
/// the integer part (whole Levenshtein units) and the low 16 bits are the
/// fractional part (accumulated ASCII weighting), printed as `%d.%05d`. We pass
/// the same `u32` distance produced by `super::distance::fuzzy_name_distance`
/// and split it identically so `--debug=FUZZY` output matches byte-for-byte.
///
/// Matches upstream rsync format:
/// ```text
/// fuzzy distance for path/to/candidate = 1.00042
/// ```
///
/// upstream: generator.c:896-899.
#[inline]
pub fn trace_fuzzy_distance(candidate: &str, distance: u32) {
    debug_log!(
        Fuzzy,
        2,
        "fuzzy distance for {} = {}.{:05}",
        candidate,
        distance >> 16,
        distance & 0xFFFF
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
            msgs.iter()
                .any(|m| m == "fuzzy basis selected for dir/report_2024.csv: dir/report_2023.csv"),
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

    /// Pins the level 2 distance emission format: the u32 distance splits into
    /// `integer.fraction` on the 16-bit boundary, exactly like upstream.
    ///
    /// upstream: generator.c:896-899.
    #[test]
    fn fuzzy_distance_matches_upstream_format() {
        setup_fuzzy(2);
        // (1 << 16) | 42 -> integer part 1, fractional part 00042.
        trace_fuzzy_distance("dir/sibling.txt", (1 << 16) | 42);
        let msgs = fuzzy_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "fuzzy distance for dir/sibling.txt = 1.00042"),
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
