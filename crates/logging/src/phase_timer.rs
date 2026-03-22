//! RAII timer for measuring transfer phase durations.
//!
//! Logs elapsed time via `debug_log!(Time, ...)` on drop, matching upstream
//! rsync's `--debug=TIME` output (upstream: options.c TIME flag).

use std::time::Instant;

/// RAII timer that logs phase duration on drop.
///
/// Create at the start of a phase; when the guard is dropped, it emits a
/// `Time` debug log with the elapsed milliseconds.
///
/// # Examples
///
/// ```rust,ignore
/// use logging::PhaseTimer;
///
/// {
///     let _t = PhaseTimer::new("file-list-build");
///     // ... build file list ...
/// } // logs "file-list-build: 42ms"
/// ```
pub struct PhaseTimer {
    name: &'static str,
    start: Instant,
}

impl PhaseTimer {
    /// Starts a new phase timer with the given name.
    ///
    /// Emits a level-2 Time debug log indicating the phase has started.
    #[must_use]
    pub fn new(name: &'static str) -> Self {
        crate::debug_log!(Time, 2, "{}: started", name);
        Self {
            name,
            start: Instant::now(),
        }
    }

    /// Returns the elapsed time in milliseconds since construction.
    #[must_use]
    pub fn elapsed_ms(&self) -> u128 {
        self.start.elapsed().as_millis()
    }
}

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        crate::debug_log!(Time, 1, "{}: {}ms", self.name, self.elapsed_ms());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_measures_elapsed() {
        let timer = PhaseTimer::new("test-phase");
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(timer.elapsed_ms() >= 10);
    }

    #[test]
    fn timer_drops_without_panic() {
        {
            let _t = PhaseTimer::new("drop-test");
        }
        // If we get here, drop didn't panic
    }

    #[test]
    fn timer_name_preserved() {
        let timer = PhaseTimer {
            name: "custom-name",
            start: Instant::now(),
        };
        assert_eq!(timer.name, "custom-name");
    }
}
