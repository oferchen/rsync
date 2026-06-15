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
//! the gap analysis that motivates this helper. SQP-LAND.4 wired the
//! helper into [`crate::io_uring`] configuration; SQP-LAND.7 added the
//! [`rootless_signal`] accessor so the SQPOLL fall-back site can log
//! which marker triggered the decision.
//!
//! ## Test override
//!
//! Setting the environment variable [`FORCE_ROOTLESS_ENV`]
//! (`OC_RSYNC_FORCE_ROOTLESS_CONTAINER`) to a truthy value (`1`, `true`,
//! `yes`, `on`, case-insensitive) makes [`rootless_signal`] report
//! [`RootlessSignal::NonIdentityUidMap`] regardless of the actual host
//! state. The override is consulted before the cached `/proc` probe, so
//! it works inside any process even if detection already ran. SQP-LAND.6
//! uses this hook to drive the SQPOLL graceful-fallback integration test
//! without requiring a real rootless Podman container in CI.

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
    rootless_signal().is_rootless()
}

/// Which detection signal triggered the rootless verdict.
///
/// Returned by [`rootless_signal`] so call sites (notably the SQPOLL
/// fall-back logger introduced in SQP-LAND.7) can surface the precise
/// reason a rootless container was detected. Operators in rootless
/// Podman / Kubernetes can then map the signal back to their environment
/// (a non-identity `/proc/self/uid_map`, a Podman `.containerenv`
/// marker, or a Docker `.dockerenv` marker) without having to grep
/// `/proc` by hand.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RootlessSignal {
    /// Not in a rootless container. SQPOLL should be attempted normally.
    NotRootless,
    /// `/proc/self/uid_map` showed a non-identity mapping - the most
    /// reliable signal that the process is inside a user namespace.
    NonIdentityUidMap,
    /// `/run/.containerenv` is present (Podman convention).
    PodmanContainerEnv,
    /// `/.dockerenv` is present (Docker convention).
    DockerEnv,
}

impl RootlessSignal {
    /// Returns a short human-readable label suitable for log output.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotRootless => "not-rootless",
            Self::NonIdentityUidMap => "non-identity-uid-map",
            Self::PodmanContainerEnv => "podman-containerenv",
            Self::DockerEnv => "docker-env",
        }
    }

    /// Returns `true` when the signal indicates a rootless container.
    #[must_use]
    pub fn is_rootless(self) -> bool {
        !matches!(self, Self::NotRootless)
    }
}

/// Returns the specific detection signal that triggered (or did not
/// trigger) the rootless verdict for this process.
///
/// Used by SQP-LAND.7 logging at the SQPOLL fall-back site so deployers
/// can see exactly which marker fired. On non-Linux targets always
/// returns [`RootlessSignal::NotRootless`].
///
/// Like [`detect_rootless_container`], this is cached after the first
/// invocation.
#[must_use]
pub fn rootless_signal() -> RootlessSignal {
    if force_rootless_via_env() {
        return RootlessSignal::NonIdentityUidMap;
    }
    imp::rootless_signal()
}

/// Environment variable that forces [`rootless_signal`] to report a
/// rootless verdict.
///
/// Setting this to `1`, `true`, `yes`, or `on` (case-insensitive) makes
/// the helper return [`RootlessSignal::NonIdentityUidMap`] without
/// consulting `/proc/self/uid_map` or any marker file. Used by the
/// SQP-LAND.6 integration test so the SQPOLL graceful-fallback path can
/// be exercised on hosts that are not actually rootless. The check runs
/// before the cached probe so toggling the variable mid-process is
/// effective even after detection already cached the host result.
pub const FORCE_ROOTLESS_ENV: &str = "OC_RSYNC_FORCE_ROOTLESS_CONTAINER";

/// Returns `true` when [`FORCE_ROOTLESS_ENV`] is set to a truthy value.
fn force_rootless_via_env() -> bool {
    match std::env::var(FORCE_ROOTLESS_ENV) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;
    use std::sync::OnceLock;

    use super::RootlessSignal;

    static CACHED: OnceLock<RootlessSignal> = OnceLock::new();

    pub(super) fn rootless_signal() -> RootlessSignal {
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
    /// container marker paths. The returned [`RootlessSignal`] names the
    /// first matching marker (or [`RootlessSignal::NotRootless`] when
    /// nothing triggered) so callers can log a precise reason.
    fn probe_with_reader<F, G>(read_uid_map: F, path_exists: G) -> RootlessSignal
    where
        F: FnOnce() -> Option<String>,
        G: Fn(&str) -> bool,
    {
        if let Some(contents) = read_uid_map()
            && !is_identity_uid_map(&contents)
        {
            return RootlessSignal::NonIdentityUidMap;
        }
        if path_exists("/run/.containerenv") {
            return RootlessSignal::PodmanContainerEnv;
        }
        if path_exists("/.dockerenv") {
            return RootlessSignal::DockerEnv;
        }
        RootlessSignal::NotRootless
    }

    /// Returns `true` when `/proc/self/uid_map` contains exactly the
    /// host-identity mapping `0 0 4294967295` (single line, whitespace
    /// tolerated) or is empty (no signal - treat as host so callers
    /// fall through to the marker-file probes). Any other content -
    /// extra lines, non-identity inner uid, narrowed range - indicates
    /// a user namespace.
    fn is_identity_uid_map(contents: &str) -> bool {
        let mut lines = contents.lines().filter(|l| !l.trim().is_empty());
        let Some(first) = lines.next() else {
            return true;
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
        use crate::container::detect_rootless_container;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[test]
        fn host_identity_uid_map_is_not_container() {
            let read = || Some(String::from("         0          0 4294967295\n"));
            let exists = |_p: &str| false;
            assert_eq!(probe_with_reader(read, exists), RootlessSignal::NotRootless);
        }

        #[test]
        fn rootless_uid_map_detected() {
            let read = || Some(String::from("         0       1000          1\n"));
            let exists = |_p: &str| false;
            assert_eq!(
                probe_with_reader(read, exists),
                RootlessSignal::NonIdentityUidMap
            );
        }

        #[test]
        fn multi_line_uid_map_detected() {
            let read = || {
                Some(String::from(
                    "         0          0 4294967295\n      1000       1000          1\n",
                ))
            };
            let exists = |_p: &str| false;
            assert_eq!(
                probe_with_reader(read, exists),
                RootlessSignal::NonIdentityUidMap
            );
        }

        #[test]
        fn truncated_range_detected() {
            let read = || Some(String::from("0 0 65536\n"));
            let exists = |_p: &str| false;
            assert_eq!(
                probe_with_reader(read, exists),
                RootlessSignal::NonIdentityUidMap
            );
        }

        #[test]
        fn missing_uid_map_falls_through_to_dockerenv() {
            let read = || None;
            let exists = |p: &str| p == "/.dockerenv";
            assert_eq!(probe_with_reader(read, exists), RootlessSignal::DockerEnv);
        }

        #[test]
        fn missing_uid_map_falls_through_to_containerenv() {
            let read = || None;
            let exists = |p: &str| p == "/run/.containerenv";
            assert_eq!(
                probe_with_reader(read, exists),
                RootlessSignal::PodmanContainerEnv
            );
        }

        #[test]
        fn no_signals_returns_false() {
            let read = || Some(String::from("         0          0 4294967295\n"));
            let exists = |_p: &str| false;
            assert_eq!(probe_with_reader(read, exists), RootlessSignal::NotRootless);
        }

        #[test]
        fn empty_uid_map_falls_through() {
            let read = || Some(String::new());
            let exists = |_p: &str| false;
            assert_eq!(probe_with_reader(read, exists), RootlessSignal::NotRootless);
        }

        #[test]
        fn uid_map_takes_priority_over_markers() {
            let read = || Some(String::from("0 1000 1\n"));
            let exists = |_p: &str| panic!("path probes must not run when uid_map signals");
            assert_eq!(
                probe_with_reader(read, exists),
                RootlessSignal::NonIdentityUidMap
            );
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
        fn containerenv_marker_takes_priority_over_dockerenv() {
            let read = || None;
            let exists = |_p: &str| true;
            assert_eq!(
                probe_with_reader(read, exists),
                RootlessSignal::PodmanContainerEnv
            );
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
    use super::RootlessSignal;

    pub(super) fn rootless_signal() -> RootlessSignal {
        RootlessSignal::NotRootless
    }

    #[cfg(test)]
    mod tests {
        use crate::container::detect_rootless_container;

        #[test]
        fn non_linux_always_returns_false() {
            assert!(!detect_rootless_container());
        }
    }
}

#[cfg(test)]
mod signal_tests {
    use super::*;

    #[test]
    fn label_is_short_and_human_readable() {
        assert_eq!(RootlessSignal::NotRootless.label(), "not-rootless");
        assert_eq!(
            RootlessSignal::NonIdentityUidMap.label(),
            "non-identity-uid-map"
        );
        assert_eq!(
            RootlessSignal::PodmanContainerEnv.label(),
            "podman-containerenv"
        );
        assert_eq!(RootlessSignal::DockerEnv.label(), "docker-env");
    }

    #[test]
    fn is_rootless_classifies_signals_correctly() {
        assert!(!RootlessSignal::NotRootless.is_rootless());
        assert!(RootlessSignal::NonIdentityUidMap.is_rootless());
        assert!(RootlessSignal::PodmanContainerEnv.is_rootless());
        assert!(RootlessSignal::DockerEnv.is_rootless());
    }
}
