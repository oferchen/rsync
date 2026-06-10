//! Rootless container / user-namespace detection for SQPOLL gating.
//!
//! io_uring SQPOLL setup (`IORING_SETUP_SQPOLL`) requires `CAP_SYS_NICE`,
//! which is structurally unavailable inside a rootless container (Podman
//! rootless, Docker with user-namespace remapping, or nested user
//! namespaces). The kernel rejects the setup syscall with `EPERM` every
//! time, costing a failed `io_uring_setup` per ring construction.
//!
//! [`detect_rootless_container`] is a pure side-effect-free helper that
//! probes the well-known environment markers so callers can skip the
//! SQPOLL attempt entirely. The probe runs at most once per process: the
//! outcome is cached in a [`OnceLock`](std::sync::OnceLock).
//!
//! Detection signals are checked in priority order; the first match
//! short-circuits:
//!
//! 1. `/proc/self/uid_map` - the most reliable signal. Inside any user
//!    namespace this file contains a non-identity mapping (typically
//!    `0 1000 1`). On a host system the mapping is always the identity
//!    `0 0 4294967295`.
//! 2. `/run/.containerenv` - created by Podman in the container mount
//!    namespace.
//! 3. `/.dockerenv` - created by Docker in the container root filesystem.
//!
//! On non-Linux targets the helper compiles to a constant `false` because
//! SQPOLL (and the entire io_uring SQPOLL safety story) is Linux-only.
//!
//! See `docs/audits/sqpoll-rootless-detection-status.md` (SQP-LAND.1) for
//! the gap analysis that motivates this helper. The future SQP-LAND.4
//! task wires the helper into [`crate::io_uring`] configuration; this
//! module ships the pure helper only - no call-site changes here.

/// Returns `true` when the current process is running inside a rootless
/// container or any user namespace where SQPOLL is structurally unable
/// to acquire `CAP_SYS_NICE`.
///
/// The probe runs at most once per process. Subsequent calls read a
/// cached result.
///
/// On non-Linux targets always returns `false`.
#[must_use]
pub fn detect_rootless_container() -> bool {
    imp::detect_rootless_container()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;
    use std::sync::OnceLock;

    static CACHED: OnceLock<bool> = OnceLock::new();

    pub(super) fn detect_rootless_container() -> bool {
        if let Some(cached) = CACHED.get().copied() {
            return cached;
        }
        let result = probe_with_reader(read_uid_map, path_exists);
        let _ = CACHED.set(result);
        CACHED.get().copied().unwrap_or(result)
    }

    fn read_uid_map() -> Option<String> {
        std::fs::read_to_string("/proc/self/uid_map").ok()
    }

    fn path_exists(p: &str) -> bool {
        Path::new(p).exists()
    }

    /// Pure detection logic with injectable I/O for unit testing.
    ///
    /// `read_uid_map` returns `Some(contents)` if `/proc/self/uid_map` is
    /// readable, `None` otherwise. `path_exists` checks the well-known
    /// container marker paths.
    fn probe_with_reader<F, G>(read_uid_map: F, path_exists: G) -> bool
    where
        F: FnOnce() -> Option<String>,
        G: Fn(&str) -> bool,
    {
        if let Some(contents) = read_uid_map()
            && !is_identity_uid_map(&contents)
        {
            return true;
        }
        if path_exists("/run/.containerenv") {
            return true;
        }
        if path_exists("/.dockerenv") {
            return true;
        }
        false
    }

    /// Returns `true` when `/proc/self/uid_map` contains exactly the
    /// host-identity mapping `0 0 4294967295` (single line, whitespace
    /// tolerated). Any other content - extra lines, non-identity inner
    /// uid, narrowed range - indicates a user namespace.
    fn is_identity_uid_map(contents: &str) -> bool {
        let mut lines = contents.lines().filter(|l| !l.trim().is_empty());
        let Some(first) = lines.next() else {
            return false;
        };
        if lines.next().is_some() {
            return false;
        }
        let mut fields = first.split_whitespace();
        let inner = fields.next();
        let outer = fields.next();
        let length = fields.next();
        let trailing = fields.next();
        matches!(
            (inner, outer, length, trailing),
            (Some("0"), Some("0"), Some("4294967295"), None)
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[test]
        fn host_identity_uid_map_is_not_container() {
            let read = || Some(String::from("         0          0 4294967295\n"));
            let exists = |_p: &str| false;
            assert!(!probe_with_reader(read, exists));
        }

        #[test]
        fn rootless_uid_map_detected() {
            let read = || Some(String::from("         0       1000          1\n"));
            let exists = |_p: &str| false;
            assert!(probe_with_reader(read, exists));
        }

        #[test]
        fn multi_line_uid_map_detected() {
            let read = || {
                Some(String::from(
                    "         0          0 4294967295\n      1000       1000          1\n",
                ))
            };
            let exists = |_p: &str| false;
            assert!(probe_with_reader(read, exists));
        }

        #[test]
        fn truncated_range_detected() {
            let read = || Some(String::from("0 0 65536\n"));
            let exists = |_p: &str| false;
            assert!(probe_with_reader(read, exists));
        }

        #[test]
        fn missing_uid_map_falls_through_to_dockerenv() {
            let read = || None;
            let exists = |p: &str| p == "/.dockerenv";
            assert!(probe_with_reader(read, exists));
        }

        #[test]
        fn missing_uid_map_falls_through_to_containerenv() {
            let read = || None;
            let exists = |p: &str| p == "/run/.containerenv";
            assert!(probe_with_reader(read, exists));
        }

        #[test]
        fn no_signals_returns_false() {
            let read = || Some(String::from("         0          0 4294967295\n"));
            let exists = |_p: &str| false;
            assert!(!probe_with_reader(read, exists));
        }

        #[test]
        fn empty_uid_map_falls_through() {
            let read = || Some(String::new());
            let exists = |_p: &str| false;
            assert!(!probe_with_reader(read, exists));
        }

        #[test]
        fn uid_map_takes_priority_over_markers() {
            let read = || Some(String::from("0 1000 1\n"));
            let exists = |_p: &str| panic!("path probes must not run when uid_map signals");
            assert!(probe_with_reader(read, exists));
        }

        #[test]
        fn read_uid_map_invoked_at_most_once() {
            let count = AtomicUsize::new(0);
            let read = || {
                count.fetch_add(1, Ordering::SeqCst);
                Some(String::from("         0          0 4294967295\n"))
            };
            let exists = |_p: &str| false;
            let _ = probe_with_reader(read, exists);
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn identity_check_accepts_trailing_newline_only() {
            assert!(is_identity_uid_map("0 0 4294967295\n"));
            assert!(is_identity_uid_map("   0   0   4294967295   \n"));
        }

        #[test]
        fn identity_check_rejects_extra_fields() {
            assert!(!is_identity_uid_map("0 0 4294967295 0\n"));
        }

        #[test]
        fn identity_check_rejects_wrong_outer_uid() {
            assert!(!is_identity_uid_map("0 1 4294967295\n"));
        }

        #[test]
        fn detect_caches_first_result() {
            let first = detect_rootless_container();
            let second = detect_rootless_container();
            assert_eq!(first, second);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub(super) fn detect_rootless_container() -> bool {
        false
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn non_linux_always_returns_false() {
            assert!(!detect_rootless_container());
        }
    }
}
