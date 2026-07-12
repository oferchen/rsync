//! Signed modification-time comparison tolerance (`--modify-window`).
//!
//! Mirrors upstream rsync's `int modify_window` (options.c:143), which the
//! generator consults through `same_time()` when deciding whether a
//! destination file is already up to date. The value is deliberately signed:
//! a negative window requests nanosecond-exact comparison rather than the
//! default whole-second tolerance.

/// Signed whole-second tolerance for the mtime quick-check.
///
/// Semantics mirror upstream `util1.c:1478 same_time()`:
/// - `0` (default): whole-second equality; any sub-second difference is ignored.
/// - `> 0`: symmetric whole-second window; nanoseconds are ignored.
/// - `< 0`: nanosecond-exact comparison - both the whole seconds and the
///   nanoseconds must be equal.
///
/// upstream: options.c:143 `int modify_window` / util1.c:1478 `same_time()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifyWindow(i64);

impl ModifyWindow {
    /// The default zero-second window (whole-second equality).
    pub const ZERO: Self = Self(0);

    /// Builds a window from a signed whole-second count.
    ///
    /// A negative `secs` selects nanosecond-exact comparison, matching
    /// upstream's `modify_window < 0` branch (util1.c:1482).
    #[must_use]
    pub const fn from_secs(secs: i64) -> Self {
        Self(secs)
    }

    /// Returns the signed whole-second window value (upstream's `int
    /// modify_window`).
    #[must_use]
    pub const fn as_secs(self) -> i64 {
        self.0
    }

    /// Reports whether the window requests nanosecond-exact comparison.
    #[must_use]
    pub const fn is_nsec_exact(self) -> bool {
        self.0 < 0
    }

    /// Returns `true` when two mtimes are "the same" under this window.
    ///
    /// Direct port of upstream `util1.c:1478 same_time()`:
    /// - `window == 0`: `f1_sec == f2_sec` (nanoseconds ignored).
    /// - `window < 0`: `f1_sec == f2_sec && f1_nsec == f2_nsec` (nsec-exact).
    /// - `window > 0`: `|f1_sec - f2_sec| <= window` (nanoseconds ignored -
    ///   upstream note: "time windows don't care about that", util1.c:1484).
    #[must_use]
    pub fn same_time(self, f1_sec: i64, f1_nsec: u32, f2_sec: i64, f2_nsec: u32) -> bool {
        // upstream: util1.c:1480
        if self.0 == 0 {
            return f1_sec == f2_sec;
        }
        // upstream: util1.c:1482 - a negative window compares nanoseconds too.
        if self.0 < 0 {
            return f1_sec == f2_sec && f1_nsec == f2_nsec;
        }
        // upstream: util1.c:1485-1487 - symmetric second window; the value is
        // positive here, so the cast to u64 for `abs_diff` is lossless.
        f1_sec.abs_diff(f2_sec) <= self.0 as u64
    }
}

#[cfg(test)]
mod tests {
    use super::ModifyWindow;

    #[test]
    fn zero_window_compares_whole_seconds_only() {
        // Why: upstream same_time() with modify_window == 0 returns
        // `f1_sec == f2_sec`, so a purely sub-second difference must still be
        // treated as equal (no needless re-transfer).
        let w = ModifyWindow::ZERO;
        assert!(w.same_time(100, 0, 100, 999_999_999));
        assert!(!w.same_time(100, 0, 101, 0));
    }

    #[test]
    fn positive_window_tolerates_second_drift_and_ignores_nsec() {
        // Why: upstream applies a symmetric whole-second window and explicitly
        // ignores nanoseconds ("time windows don't care about that").
        let w = ModifyWindow::from_secs(2);
        assert!(w.same_time(100, 0, 102, 0));
        assert!(w.same_time(102, 0, 100, 0));
        assert!(!w.same_time(100, 0, 103, 0));
        assert!(w.same_time(100, 500, 102, 999)); // nsec ignored
    }

    #[test]
    fn negative_window_requires_nanosecond_exactness() {
        // Why: `--modify-window=-1` selects upstream's `modify_window < 0`
        // branch, where two files differing only in the sub-second component
        // are considered DIFFERENT and must be transferred.
        let w = ModifyWindow::from_secs(-1);
        assert!(w.is_nsec_exact());
        assert!(w.same_time(100, 500, 100, 500));
        assert!(!w.same_time(100, 500, 100, 501));
        assert!(!w.same_time(100, 0, 101, 0));
    }
}
