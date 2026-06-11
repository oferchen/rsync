//! io_uring configuration, kernel detection, and availability caching.
//!
//! Kernel version detection uses `uname(2)` to parse the release string and
//! requires >= 5.6. The result is cached in process-wide atomics so that
//! subsequent calls to [`is_io_uring_available`] are a single relaxed load.

use std::ffi::CStr;
use std::io;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use io_uring::IoUring as RawIoUring;

use crate::container::rootless_signal;

/// Minimum kernel version required for io_uring.
///
/// Linux 5.6 introduced `io_uring_setup(2)` with support for all opcodes this
/// crate uses: `IORING_OP_READ`, `IORING_OP_WRITE`, `IORING_OP_SEND`,
/// `IORING_REGISTER_FILES`, and `IORING_SETUP_SQPOLL`. Earlier kernels (5.1-5.5)
/// had partial io_uring support but lacked critical features.
const MIN_KERNEL_VERSION: (u32, u32) = (5, 6);

/// Cached result of io_uring availability check.
static IO_URING_AVAILABLE: AtomicBool = AtomicBool::new(false);
static IO_URING_CHECKED: AtomicBool = AtomicBool::new(false);

/// Environment variable that forces io_uring availability to report `false`.
///
/// When set to a truthy value (`1`, `true`, `yes`, case-insensitive),
/// [`is_io_uring_available`] returns `false` regardless of kernel support.
/// Used to exercise the standard-I/O fallback path on hosts that would
/// otherwise satisfy the kernel probe, including CI runners on older
/// kernels and emulators.
///
/// The check is a single environment lookup per call. When the variable is
/// unset (the production default), behaviour is unchanged.
pub(crate) const DISABLE_ENV: &str = "OC_RSYNC_DISABLE_IOURING";

/// Returns `true` when [`DISABLE_ENV`] is set to a truthy value.
fn disable_via_env() -> bool {
    match std::env::var(DISABLE_ENV) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

/// Whether SQPOLL was requested but fell back to regular submission.
///
/// Set to `true` the first time `build_ring()` attempts SQPOLL and it fails
/// (typically `EPERM` because the process lacks `CAP_SYS_NICE`). Callers
/// can query this via [`sqpoll_fell_back`] for diagnostics or `--version` output.
static SQPOLL_FALLBACK: AtomicBool = AtomicBool::new(false);

/// Process-wide gate that suppresses every `IORING_SETUP_SQPOLL` request.
///
/// Set by [`set_sqpoll_disabled_by_policy`] when the CLI parser sees
/// `--no-io-uring-sqpoll`. Every ring construction site
/// (`IoUringConfig::build_ring` and the session-pool ring builder in
/// `session_pool.rs`) consults this flag before calling
/// `io_uring::IoUring::builder().setup_sqpoll(...)`. When set, the SQPOLL
/// kthread is never requested, even if a particular `IoUringConfig` has
/// `sqpoll: true`. The opt-out is one-way per process - there is no
/// matching unset, since toggling SQPOLL on for an in-flight transfer
/// would race the ring builder.
static SQPOLL_DISABLED_BY_POLICY: AtomicBool = AtomicBool::new(false);

/// Returns `true` when [`set_sqpoll_disabled_by_policy`] has been called for
/// this process.
///
/// Ring-construction sites use this to short-circuit SQPOLL requests
/// regardless of any per-config `sqpoll: true` setting. Kept `pub` so
/// callers outside the `io_uring::config` module (the session pool builder
/// in `session_pool.rs` and the `--io-uring-status` reporter in
/// `status.rs`) can read the gate without re-exporting the atomic.
#[must_use]
pub fn is_sqpoll_disabled_by_policy() -> bool {
    SQPOLL_DISABLED_BY_POLICY.load(Ordering::Relaxed)
}

/// Records the process-wide opt-out from SQPOLL.
///
/// Once called, every subsequent `build_ring()` call (and every
/// session-pool ring builder) honours
/// [`is_sqpoll_disabled_by_policy`] and skips the
/// `IORING_SETUP_SQPOLL` flag. The CLI parser invokes this when the user
/// passes `--no-io-uring-sqpoll`. The function is idempotent and one-way
/// per process to keep the gate race-free against ring construction on
/// other threads.
pub fn set_sqpoll_disabled_by_policy() {
    SQPOLL_DISABLED_BY_POLICY.store(true, Ordering::Relaxed);
}

/// One-shot guard so the rootless-fallback log emits once per process.
///
/// SQP-LAND.7 wires [`rootless_signal`] into ring construction so we can
/// skip a doomed SQPOLL setup syscall in rootless containers. The log
/// itself is informational (configuration decision, not an error), but
/// daemon workloads build many rings per process and we do not want to
/// flood operator logs - hence the [`Once`] gate.
static ROOTLESS_SQPOLL_LOG_ONCE: Once = Once::new();

/// Skip SQPOLL when the process is inside a rootless container.
///
/// `CAP_SYS_NICE` is structurally unavailable inside a user namespace
/// (rootless Podman, Docker with `--userns=...`, Kubernetes with a
/// `runAsNonRoot` securityContext), so requesting
/// `IORING_SETUP_SQPOLL` is guaranteed to fail with `EPERM` and leaves
/// the operator wondering why io_uring performance is degraded. We
/// short-circuit here and emit one structured info-level log per
/// process so deployers can map the verdict back to their environment.
///
/// Returns `true` when SQPOLL must be suppressed for this build. The
/// caller stays on the plain-ring path.
///
/// See SQP-LAND series (SQP-LAND.3 helper, SQP-LAND.4 wiring,
/// SQP-LAND.7 observability) and [`crate::container`] for the
/// detection logic.
fn should_skip_sqpoll_due_to_rootless() -> bool {
    rootless_skip_decision(
        rootless_signal(),
        &ROOTLESS_SQPOLL_LOG_ONCE,
        log_rootless_skip,
    )
}

/// Pure decision helper for SQP-LAND.7: takes a signal + a one-shot
/// guard + a log-emitter and returns whether SQPOLL must be skipped.
///
/// Split out from [`should_skip_sqpoll_due_to_rootless`] so unit tests
/// can drive every code path (not-rootless / once-fires /
/// already-fired) without depending on the process-cached
/// [`rootless_signal`].
fn rootless_skip_decision<L>(signal: crate::container::RootlessSignal, once: &Once, log: L) -> bool
where
    L: FnOnce(crate::container::RootlessSignal),
{
    if !signal.is_rootless() {
        return false;
    }
    once.call_once(|| log(signal));
    true
}

/// Default log emitter for the rootless-fallback decision.
///
/// Mirrors the IKV-F.1 fallback-log convention (Io target, level 1) so
/// operators see this alongside the other io_uring decisions under
/// `--debug=io1`. Kept as a free function so [`rootless_skip_decision`]
/// can drive it from unit tests via a substitute closure.
fn log_rootless_skip(signal: crate::container::RootlessSignal) {
    // SQP-LAND.7: deployer-facing rationale for the SQPOLL skip.
    logging::debug_log!(
        Io,
        1,
        "io_uring SQPOLL disabled: rootless container detected (signal={}, no CAP_SYS_NICE \
         available in this user namespace); falling back to standard polling",
        signal.label()
    );
}

/// Returns `true` if SQPOLL was requested but setup failed.
///
/// When `IoUringConfig::sqpoll` is `true` but the kernel rejects the request
/// (usually `EPERM` due to missing `CAP_SYS_NICE`), `build_ring()` transparently
/// falls back to a regular io_uring ring. This function reports whether that
/// fallback occurred, enabling diagnostic output like:
///
/// ```text
/// io_uring SQPOLL requires CAP_SYS_NICE, fell back to regular submission
/// ```
///
/// Returns `false` if SQPOLL was never requested or if it succeeded.
#[must_use]
pub fn sqpoll_fell_back() -> bool {
    SQPOLL_FALLBACK.load(Ordering::Relaxed)
}

/// Parses kernel version from uname release string (e.g., "5.15.0-generic").
pub(super) fn parse_kernel_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Gets the kernel release string using libc uname.
fn get_kernel_release() -> Option<String> {
    // SAFETY: `utsname` is zero-initialised then fully populated by `uname`,
    // which is sound for the POD struct; the release field is a NUL-terminated
    // byte array that remains valid for the duration of the `CStr` borrow.
    unsafe {
        let mut utsname: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut utsname) != 0 {
            return None;
        }
        let release = CStr::from_ptr(utsname.release.as_ptr());
        release.to_str().ok().map(String::from)
    }
}

pub use crate::io_uring_common::IoUringKernelInfo;

/// Public accessors for kernel version detection used by `--version` output.
pub mod config_detail {
    /// Parses kernel version from uname release string (e.g., "5.15.0-generic").
    pub fn parse_kernel_version(release: &str) -> Option<(u32, u32)> {
        super::parse_kernel_version(release)
    }

    /// Returns the kernel release string from `uname(2)`.
    pub fn get_kernel_release_string() -> Option<String> {
        super::get_kernel_release()
    }

    /// Returns a human-readable reason for io_uring availability or unavailability.
    ///
    /// Probes the kernel version and attempts to create a minimal io_uring
    /// instance, returning a log-friendly string describing the result.
    ///
    /// Example outputs:
    /// - `"io_uring: enabled (kernel 6.1, 48 ops supported)"`
    /// - `"io_uring: disabled (kernel 4.19 < 5.6 required)"`
    /// - `"io_uring: disabled (kernel 5.15, io_uring_setup(2) blocked by seccomp, container, or permission restriction)"`
    #[must_use]
    pub fn io_uring_availability_reason() -> String {
        super::check_io_uring_reason().reason()
    }

    /// Returns structured kernel information for io_uring availability.
    ///
    /// Probes the kernel version and io_uring syscall availability, returning
    /// a struct with machine-readable fields for programmatic consumption.
    /// The PBUF_RING capability flag reflects the cached
    /// [`crate::io_uring::buffer_ring::pbuf_ring_supported`] probe.
    #[must_use]
    pub fn io_uring_kernel_info() -> super::IoUringKernelInfo {
        let result = super::check_io_uring_reason();
        let pbuf_ring_supported = crate::io_uring::buffer_ring::pbuf_ring_supported();
        match &result {
            super::IoUringProbeResult::Available {
                major,
                minor,
                supported_ops,
            } => super::IoUringKernelInfo {
                available: true,
                kernel_major: Some(*major),
                kernel_minor: Some(*minor),
                supported_ops: *supported_ops,
                pbuf_ring_supported,
                reason: result.reason(),
            },
            super::IoUringProbeResult::KernelTooOld { major, minor }
            | super::IoUringProbeResult::SyscallBlocked { major, minor } => {
                super::IoUringKernelInfo {
                    available: false,
                    kernel_major: Some(*major),
                    kernel_minor: Some(*minor),
                    supported_ops: 0,
                    pbuf_ring_supported,
                    reason: result.reason(),
                }
            }
            super::IoUringProbeResult::NoKernelRelease
            | super::IoUringProbeResult::UnparsableVersion => super::IoUringKernelInfo {
                available: false,
                kernel_major: None,
                kernel_minor: None,
                supported_ops: 0,
                pbuf_ring_supported,
                reason: result.reason(),
            },
        }
    }
}

/// Checks if the current kernel supports io_uring.
///
/// Returns `true` if all of the following hold:
///
/// 1. Running on Linux
/// 2. Kernel version is 5.6 or later (parsed from `uname().release`)
/// 3. `io_uring_setup(2)` succeeds - not blocked by seccomp or container runtime
/// 4. The `DISABLE_ENV` (`OC_RSYNC_DISABLE_IOURING`) environment variable is unset (or not truthy)
///
/// The kernel probe result is cached after the first call. The environment
/// variable is consulted on every call so tests and operators can force the
/// standard-I/O fallback at runtime without restarting the process. When the
/// variable is unset the additional check is a single failed `getenv`.
#[must_use]
pub fn is_io_uring_available() -> bool {
    if disable_via_env() {
        return false;
    }

    if IO_URING_CHECKED.load(Ordering::Relaxed) {
        return IO_URING_AVAILABLE.load(Ordering::Relaxed);
    }

    let result = check_io_uring_reason();
    let reason = result.reason();
    let available = matches!(result, IoUringProbeResult::Available { .. });
    IO_URING_AVAILABLE.store(available, Ordering::Relaxed);
    IO_URING_CHECKED.store(true, Ordering::Relaxed);
    logging::debug_log!(Io, 1, "{reason}");
    available
}

/// Result of probing io_uring availability with the specific reason.
#[derive(Debug, Clone)]
pub(crate) enum IoUringProbeResult {
    /// io_uring is available on this kernel.
    Available {
        /// Detected kernel major.minor version.
        major: u32,
        minor: u32,
        /// Number of supported io_uring opcodes reported by `IORING_REGISTER_PROBE`.
        supported_ops: u32,
    },
    /// Could not read the kernel release string from uname(2).
    NoKernelRelease,
    /// Kernel release string could not be parsed into major.minor.
    UnparsableVersion,
    /// Kernel version is below the 5.6 minimum.
    KernelTooOld {
        /// Detected kernel major.minor version.
        major: u32,
        minor: u32,
    },
    /// Kernel version is sufficient but io_uring_setup(2) failed - likely
    /// blocked by seccomp, container runtime, or permission restrictions.
    SyscallBlocked {
        /// Detected kernel major.minor version.
        major: u32,
        minor: u32,
    },
}

impl IoUringProbeResult {
    /// Returns a human-readable reason string suitable for log output.
    ///
    /// For the [`Available`](Self::Available) variant the suffix
    /// `, pbuf_ring=yes` or `, pbuf_ring=no` reflects the cached
    /// [`super::buffer_ring::pbuf_ring_supported`] probe so that
    /// `--version` output can communicate which fallback tier is in use.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Available {
                major,
                minor,
                supported_ops,
            } => {
                let pbuf = if super::buffer_ring::pbuf_ring_supported() {
                    "yes"
                } else {
                    "no"
                };
                format!(
                    "io_uring: enabled (kernel {major}.{minor}, {supported_ops} ops supported, \
                     pbuf_ring={pbuf})"
                )
            }
            Self::NoKernelRelease => {
                "io_uring: disabled (could not read kernel version)".to_string()
            }
            Self::UnparsableVersion => {
                "io_uring: disabled (could not parse kernel version)".to_string()
            }
            Self::KernelTooOld { major, minor } => {
                format!("io_uring: disabled (kernel {major}.{minor} < 5.6 required)")
            }
            Self::SyscallBlocked { major, minor } => {
                format!(
                    "io_uring: disabled (kernel {major}.{minor}, io_uring_setup(2) blocked \
                     by seccomp, container, or permission restriction)"
                )
            }
        }
    }
}

/// Counts supported io_uring opcodes by probing via `IORING_REGISTER_PROBE`.
///
/// Creates a temporary ring, registers a probe, and counts how many opcodes
/// the kernel reports as supported. Returns 0 if the probe fails.
fn count_supported_ops(ring: &RawIoUring) -> u32 {
    let mut probe = io_uring::Probe::new();
    if ring.submitter().register_probe(&mut probe).is_ok() {
        (0..=u8::MAX).filter(|&op| probe.is_supported(op)).count() as u32
    } else {
        0
    }
}

/// Probes io_uring availability and returns the detailed result.
pub(crate) fn check_io_uring_reason() -> IoUringProbeResult {
    let release = match get_kernel_release() {
        Some(r) => r,
        None => return IoUringProbeResult::NoKernelRelease,
    };

    let (major, minor) = match parse_kernel_version(&release) {
        Some(v) => v,
        None => return IoUringProbeResult::UnparsableVersion,
    };

    if (major, minor) < MIN_KERNEL_VERSION {
        return IoUringProbeResult::KernelTooOld { major, minor };
    }

    match RawIoUring::new(4) {
        Ok(ring) => {
            let supported_ops = count_supported_ops(&ring);
            IoUringProbeResult::Available {
                major,
                minor,
                supported_ops,
            }
        }
        Err(_) => IoUringProbeResult::SyscallBlocked { major, minor },
    }
}

pub use crate::io_uring_common::IoUringConfig;

/// Linux-only ring-construction methods for the shared [`IoUringConfig`].
///
/// The plain-data struct lives in `io_uring_common` so the
/// non-Linux stub can expose the identical field layout without duplicating
/// the definition. The `build_ring` method below is the only platform-gated
/// behaviour: it requires the `io_uring` crate which is Linux-only.
impl IoUringConfig {
    /// Builds an `IoUring` instance from this config.
    ///
    /// Tries SQPOLL first if requested; falls back to a plain ring on
    /// `EPERM` / `ENOMEM`. This two-step approach means callers can
    /// optimistically request SQPOLL without needing privilege checks
    /// upfront - the fallback is transparent.
    ///
    /// # Defensive SQPOLL + mmap refusal (Candidate 3 backstop)
    ///
    /// When [`mmap_basis_active`](Self::mmap_basis_active) is set the
    /// caller is signalling that an `MmapReader` / `MmapStrategy` is live
    /// on the same transfer plan that owns this ring. Pairing SQPOLL with
    /// a file-backed mmap region is a documented kernel hazard: the SQPOLL
    /// kthread services SQEs without the user `mm` context, so cold-page
    /// faults on mapped pages bounce to `task_work` on the original task
    /// (deadlock loop on pre-6.x kernels) and concurrent truncation
    /// surfaces as in-kernel `SIGBUS`. See
    /// `docs/audits/io-uring-sqpoll-mmap-interaction.md` for the long-form
    /// reasoning.
    ///
    /// # SQM-3: mlock'd basis window allows SQPOLL+mmap pairing
    ///
    /// With the `sqpoll-mlock-basis` feature on (default), the per-SQE-batch
    /// [`crate::sqpoll_basis::WiredBasisWindow`] primitive pins the mmap'd
    /// basis range via `mlock(2)` before each submission. The wired pages
    /// cannot fault from the SQPOLL kthread, so the race surface that
    /// motivates the defensive refusal is closed structurally. When the
    /// feature is off (operator opt-out per the SQM-2.b rollback path), or
    /// the wiring fails with a downgrade-class errno at submission time, the
    /// refusal here remains the safety net: the ring is built without
    /// SQPOLL and `SQPOLL_FALLBACK` is set so callers see the degrade.
    pub(crate) fn build_ring(&self) -> io::Result<RawIoUring> {
        let policy_disabled = is_sqpoll_disabled_by_policy();
        let rootless_skip = self.sqpoll && !policy_disabled && should_skip_sqpoll_due_to_rootless();
        let sqpoll_requested = self.sqpoll && !policy_disabled && !rootless_skip;
        if self.sqpoll && policy_disabled {
            logging::debug_log!(
                Io,
                1,
                "io_uring: SQPOLL suppressed by --no-io-uring-sqpoll; \
                 building a regular ring (BGID and other features remain active)"
            );
        }
        if rootless_skip {
            // SQP-LAND.7: record the structural skip so callers querying
            // sqpoll_fell_back() see the same "ran without SQPOLL" verdict
            // they would observe after a kernel EPERM.
            SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
        }
        let mlock_basis_enabled = cfg!(feature = "sqpoll-mlock-basis");
        let sqpoll_safe = sqpoll_requested && (!self.mmap_basis_active || mlock_basis_enabled);
        if sqpoll_requested && !sqpoll_safe {
            logging::debug_log!(
                Io,
                1,
                "io_uring: refusing SQPOLL because an mmap basis reader is active on this \
                 transfer plan and the sqpoll-mlock-basis feature is off (SQPOLL kthread + \
                 file-backed mmap is a known kernel deadlock hazard on pre-6.x kernels); \
                 falling back to a regular ring"
            );
            SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
        }
        if sqpoll_safe {
            let mut builder = io_uring::IoUring::builder();
            builder.setup_sqpoll(self.sqpoll_idle_ms);
            match builder.build(self.sq_entries) {
                Ok(ring) => return Ok(ring),
                Err(_) => {
                    // SQPOLL requires CAP_SYS_NICE on most kernels. Record
                    // the fallback so callers can surface it in diagnostics.
                    SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
                }
            }
        }
        RawIoUring::new(self.sq_entries)
            .map_err(|e| io::Error::other(format!("io_uring init failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_result_available_reason_contains_kernel_version_and_ops() {
        let result = IoUringProbeResult::Available {
            major: 6,
            minor: 1,
            supported_ops: 48,
        };
        let reason = result.reason();
        assert!(reason.contains("enabled"));
        assert!(reason.contains("6.1"));
        assert!(reason.contains("48 ops supported"));
        assert!(
            reason.contains("pbuf_ring=yes") || reason.contains("pbuf_ring=no"),
            "available reason must surface PBUF_RING probe result, got: {reason}"
        );
    }

    #[test]
    fn probe_result_no_kernel_release_reason() {
        let result = IoUringProbeResult::NoKernelRelease;
        let reason = result.reason();
        assert!(reason.contains("disabled"));
        assert!(reason.contains("could not read kernel version"));
    }

    #[test]
    fn probe_result_unparsable_version_reason() {
        let result = IoUringProbeResult::UnparsableVersion;
        let reason = result.reason();
        assert!(reason.contains("disabled"));
        assert!(reason.contains("could not parse kernel version"));
    }

    #[test]
    fn probe_result_kernel_too_old_reason() {
        let result = IoUringProbeResult::KernelTooOld {
            major: 4,
            minor: 19,
        };
        let reason = result.reason();
        assert!(reason.contains("disabled"));
        assert!(reason.contains("4.19"));
        assert!(reason.contains("< 5.6 required"));
    }

    #[test]
    fn probe_result_syscall_blocked_reason() {
        let result = IoUringProbeResult::SyscallBlocked {
            major: 5,
            minor: 15,
        };
        let reason = result.reason();
        assert!(reason.contains("disabled"));
        assert!(reason.contains("5.15"));
        assert!(reason.contains("blocked"));
        assert!(reason.contains("seccomp"));
    }

    #[test]
    fn probe_result_all_variants_start_with_io_uring_prefix() {
        let variants: Vec<IoUringProbeResult> = vec![
            IoUringProbeResult::Available {
                major: 6,
                minor: 8,
                supported_ops: 50,
            },
            IoUringProbeResult::NoKernelRelease,
            IoUringProbeResult::UnparsableVersion,
            IoUringProbeResult::KernelTooOld { major: 4, minor: 0 },
            IoUringProbeResult::SyscallBlocked {
                major: 5,
                minor: 10,
            },
        ];

        for variant in &variants {
            let reason = variant.reason();
            assert!(
                reason.starts_with("io_uring: "),
                "all variants must start with 'io_uring: ' prefix, got: {reason}"
            );
            assert!(
                !reason.contains('\n'),
                "reason must be single line, got: {reason}"
            );
        }
    }

    #[test]
    fn kernel_info_available_has_all_fields() {
        let info = IoUringKernelInfo {
            available: true,
            kernel_major: Some(6),
            kernel_minor: Some(1),
            supported_ops: 48,
            pbuf_ring_supported: true,
            reason: "io_uring: enabled (kernel 6.1, 48 ops supported, pbuf_ring=yes)".to_string(),
        };
        assert!(info.available);
        assert_eq!(info.kernel_major, Some(6));
        assert_eq!(info.kernel_minor, Some(1));
        assert!(info.supported_ops > 0);
        assert!(info.pbuf_ring_supported);
    }

    #[test]
    fn kernel_info_unavailable_has_zero_ops() {
        let info = IoUringKernelInfo {
            available: false,
            kernel_major: Some(4),
            kernel_minor: Some(19),
            supported_ops: 0,
            pbuf_ring_supported: false,
            reason: "io_uring: disabled (kernel 4.19 < 5.6 required)".to_string(),
        };
        assert!(!info.available);
        assert_eq!(info.supported_ops, 0);
        assert!(!info.pbuf_ring_supported);
    }

    #[test]
    fn kernel_info_no_kernel_release_has_none_versions() {
        let info = IoUringKernelInfo {
            available: false,
            kernel_major: None,
            kernel_minor: None,
            supported_ops: 0,
            pbuf_ring_supported: false,
            reason: "io_uring: disabled (could not read kernel version)".to_string(),
        };
        assert!(!info.available);
        assert!(info.kernel_major.is_none());
        assert!(info.kernel_minor.is_none());
    }

    #[test]
    fn config_detail_kernel_info_returns_consistent_result() {
        let info = config_detail::io_uring_kernel_info();
        let reason = config_detail::io_uring_availability_reason();
        assert_eq!(info.reason, reason);
        assert_eq!(info.available, is_io_uring_available());
    }

    #[test]
    fn sqpoll_fell_back_initial_state() {
        // The SQPOLL_FALLBACK atomic starts as false. It is only set to true
        // when build_ring() attempts SQPOLL and it fails.
        assert!(!sqpoll_fell_back());
    }

    #[test]
    fn parse_kernel_version_valid_strings() {
        assert_eq!(parse_kernel_version("5.6.0"), Some((5, 6)));
        assert_eq!(parse_kernel_version("5.15.0-generic"), Some((5, 15)));
        assert_eq!(parse_kernel_version("6.1.0"), Some((6, 1)));
        assert_eq!(parse_kernel_version("4.19.123-aws"), Some((4, 19)));
    }

    #[test]
    fn parse_kernel_version_invalid_strings() {
        assert_eq!(parse_kernel_version("invalid"), None);
        assert_eq!(parse_kernel_version(""), None);
    }

    #[test]
    fn config_detail_io_uring_availability_reason_is_non_empty() {
        let reason = config_detail::io_uring_availability_reason();
        assert!(!reason.is_empty());
        assert!(reason.starts_with("io_uring: "));
    }

    #[test]
    fn parse_kernel_version_extra_dots_azure() {
        // Azure kernel strings have extra dot-separated segments.
        assert_eq!(parse_kernel_version("5.15.0.1-azure"), Some((5, 15)));
    }

    #[test]
    fn parse_kernel_version_very_large_numbers() {
        assert_eq!(parse_kernel_version("100.200.300"), Some((100, 200)));
    }

    #[test]
    fn parse_kernel_version_single_digit_returns_none() {
        // A single digit has no minor component - the second `parts.next()?`
        // yields an empty string from the trailing split, which fails to parse.
        assert_eq!(parse_kernel_version("5"), None);
    }

    #[test]
    fn parse_kernel_version_trailing_rc_suffix() {
        // Release candidate strings like "6.1.0-rc1" - the split on non-digit
        // chars separates "rc1" from the numeric parts.
        assert_eq!(parse_kernel_version("6.1.0-rc1"), Some((6, 1)));
    }

    #[test]
    fn parse_kernel_version_leading_zeros() {
        // Rust's u32::parse treats leading zeros as valid decimal.
        assert_eq!(parse_kernel_version("06.01.00"), Some((6, 1)));
    }

    #[test]
    fn parse_kernel_version_zero_zero() {
        assert_eq!(parse_kernel_version("0.0.0"), Some((0, 0)));
    }

    #[test]
    fn parse_kernel_version_wsl_style() {
        // WSL2 kernel: "5.15.167.4-microsoft-standard-WSL2"
        assert_eq!(
            parse_kernel_version("5.15.167.4-microsoft-standard-WSL2"),
            Some((5, 15))
        );
    }

    #[test]
    fn parse_kernel_version_chromeos_style() {
        // ChromeOS: "5.10.159-20950-g5765b1ef511a"
        assert_eq!(
            parse_kernel_version("5.10.159-20950-g5765b1ef511a"),
            Some((5, 10))
        );
    }

    fn is_power_of_two(n: u32) -> bool {
        n > 0 && (n & (n - 1)) == 0
    }

    #[test]
    fn default_config_sq_entries_is_power_of_two() {
        let config = IoUringConfig::default();
        assert!(
            is_power_of_two(config.sq_entries),
            "default sq_entries {} must be a power of 2",
            config.sq_entries
        );
    }

    #[test]
    fn large_files_config_has_reasonable_values() {
        let config = IoUringConfig::for_large_files();
        assert!(
            is_power_of_two(config.sq_entries),
            "sq_entries {} must be a power of 2",
            config.sq_entries
        );
        assert!(
            config.sq_entries >= 64,
            "large file config should have at least 64 SQ entries"
        );
        assert!(
            config.buffer_size >= 128 * 1024,
            "large file buffer should be at least 128 KB"
        );
        assert!(
            config.buffer_size <= 4 * 1024 * 1024,
            "large file buffer should not exceed 4 MB"
        );
        assert!(config.register_files, "fd registration should be enabled");
        assert!(
            config.register_buffers,
            "buffer registration should be enabled for large files"
        );
        assert!(
            config.registered_buffer_count >= 8,
            "large file config should register at least 8 buffers"
        );
    }

    #[test]
    fn small_files_config_has_reasonable_values() {
        let config = IoUringConfig::for_small_files();
        assert!(
            is_power_of_two(config.sq_entries),
            "sq_entries {} must be a power of 2",
            config.sq_entries
        );
        assert!(
            config.buffer_size >= 4 * 1024,
            "small file buffer should be at least 4 KB"
        );
        assert!(
            config.buffer_size <= 128 * 1024,
            "small file buffer should not exceed 128 KB"
        );
        assert!(config.register_files, "fd registration should be enabled");
    }

    #[test]
    fn small_files_config_has_smaller_buffers_than_large() {
        let small = IoUringConfig::for_small_files();
        let large = IoUringConfig::for_large_files();
        assert!(
            small.buffer_size < large.buffer_size,
            "small file buffer ({}) should be smaller than large file buffer ({})",
            small.buffer_size,
            large.buffer_size
        );
    }

    #[test]
    fn large_files_config_has_more_sq_entries_than_default() {
        let default = IoUringConfig::default();
        let large = IoUringConfig::for_large_files();
        assert!(
            large.sq_entries >= default.sq_entries,
            "large file sq_entries ({}) should be >= default ({})",
            large.sq_entries,
            default.sq_entries
        );
    }

    #[test]
    fn default_config_sqpoll_disabled() {
        let config = IoUringConfig::default();
        assert!(!config.sqpoll, "SQPOLL should be disabled by default");
    }

    #[test]
    fn build_ring_with_sqpoll_falls_back_gracefully() {
        // Request SQPOLL - on most CI machines without CAP_SYS_NICE this will
        // fail and fall back to a regular ring. Either way, build_ring() must
        // succeed.
        let config = IoUringConfig {
            sqpoll: true,
            ..IoUringConfig::default()
        };
        let ring_result = config.build_ring();
        assert!(
            ring_result.is_ok(),
            "build_ring() must succeed even when SQPOLL falls back: {:?}",
            ring_result.err()
        );
    }

    #[test]
    fn sqpoll_fallback_flag_set_after_failed_sqpoll() {
        // Reset the global to a known state - note: this is not thread-safe
        // but test runners serialize by default.
        SQPOLL_FALLBACK.store(false, Ordering::Relaxed);

        let config = IoUringConfig {
            sqpoll: true,
            ..IoUringConfig::default()
        };
        let _ = config.build_ring();

        // On unprivileged systems, SQPOLL setup fails and the flag is set.
        // On privileged systems (root/CAP_SYS_NICE), SQPOLL succeeds and the
        // flag stays false. Both outcomes are valid.
        // We cannot assert a specific value since it depends on privileges,
        // but we can verify the flag is queryable without panic.
        let _fell_back: bool = sqpoll_fell_back();
    }

    #[test]
    fn build_ring_without_sqpoll_does_not_set_fallback() {
        SQPOLL_FALLBACK.store(false, Ordering::Relaxed);

        let config = IoUringConfig::default();
        assert!(!config.sqpoll);
        let _ = config.build_ring();

        assert!(
            !sqpoll_fell_back(),
            "SQPOLL fallback flag must not be set when SQPOLL was not requested"
        );
    }

    #[test]
    fn default_config_mmap_basis_inactive() {
        let config = IoUringConfig::default();
        assert!(
            !config.mmap_basis_active,
            "mmap_basis_active must default to false"
        );
        assert!(!IoUringConfig::for_large_files().mmap_basis_active);
        assert!(!IoUringConfig::for_small_files().mmap_basis_active);
    }

    #[test]
    fn build_ring_sqpoll_with_small_files_no_mmap_keeps_request() {
        // Small-file plan: no mmap basis, SQPOLL request must reach the
        // builder (and either succeed or set the fallback flag, both fine).
        SQPOLL_FALLBACK.store(false, Ordering::Relaxed);
        let config = IoUringConfig {
            sqpoll: true,
            mmap_basis_active: false,
            ..IoUringConfig::for_small_files()
        };
        let ring = config.build_ring();
        assert!(
            ring.is_ok(),
            "build_ring() must succeed when no mmap basis is active"
        );
        // The fallback flag may or may not be set depending on whether the
        // test runner has CAP_SYS_NICE; only the no-mmap path even attempts
        // setup_sqpoll, so simply succeeding is the assertion we need.
    }

    #[test]
    fn build_ring_sqpoll_with_mmap_basis_respects_mlock_feature() {
        // Large-file plan with mmap basis: behaviour depends on whether the
        // SQM-3 sqpoll-mlock-basis feature is compiled in. With the feature
        // on (default), build_ring keeps SQPOLL because per-submission mlock
        // closes the race surface. With the feature off, the defensive
        // refusal still trips and the fallback flag is set.
        SQPOLL_FALLBACK.store(false, Ordering::Relaxed);
        let config = IoUringConfig {
            sqpoll: true,
            mmap_basis_active: true,
            ..IoUringConfig::for_large_files()
        };
        let ring = config.build_ring();
        assert!(
            ring.is_ok(),
            "build_ring() must succeed regardless of feature gating"
        );
        if cfg!(feature = "sqpoll-mlock-basis") {
            // Feature on: SQPOLL is allowed; the fallback flag is only set
            // if the kernel itself rejects SQPOLL (no CAP_SYS_NICE). On
            // unprivileged CI runners that still trips; on privileged hosts
            // it does not. Both outcomes are valid for this assertion, so
            // we only verify the flag is queryable.
            let _ = sqpoll_fell_back();
        } else {
            assert!(
                sqpoll_fell_back(),
                "SQPOLL fallback flag must be set when mmap_basis_active and \
                 sqpoll-mlock-basis is off"
            );
        }
    }

    #[test]
    fn sqpoll_fallback_logs_rootless_reason() {
        // SQP-LAND.7: when the rootless detector trips, the decision
        // helper must short-circuit SQPOLL, invoke the log emitter
        // exactly once, and report the precise signal that fired.
        use crate::container::RootlessSignal;
        use std::sync::atomic::{AtomicU32, AtomicUsize};

        let once = Once::new();
        let invocations = AtomicUsize::new(0);
        let captured = AtomicU32::new(0);
        let log = |signal: RootlessSignal| {
            invocations.fetch_add(1, Ordering::SeqCst);
            captured.store(signal as u32, Ordering::SeqCst);
        };

        let skipped = rootless_skip_decision(RootlessSignal::NonIdentityUidMap, &once, log);

        assert!(skipped, "rootless verdict must skip SQPOLL");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "log emitter must fire exactly once on the first rootless decision"
        );
        assert_eq!(
            captured.load(Ordering::SeqCst),
            RootlessSignal::NonIdentityUidMap as u32,
            "captured signal must match the precise marker that triggered the verdict"
        );

        // A second invocation through the same Once must not re-fire the
        // log emitter - this guards against daemon-log flooding.
        let log_again = |_signal: RootlessSignal| {
            panic!("log emitter must fire at most once per process");
        };
        let skipped_again =
            rootless_skip_decision(RootlessSignal::PodmanContainerEnv, &once, log_again);
        assert!(
            skipped_again,
            "every rootless verdict must still skip SQPOLL"
        );
    }

    #[test]
    fn sqpoll_fallback_skips_log_when_not_rootless() {
        // The non-rootless path is the hot path on bare-metal hosts: it
        // must not emit any log or even consume the Once guard, so a
        // later rootless verdict on the same process can still log.
        use crate::container::RootlessSignal;

        let once = Once::new();
        let log = |_signal: RootlessSignal| {
            panic!("log emitter must not fire when signal=NotRootless");
        };
        let skipped = rootless_skip_decision(RootlessSignal::NotRootless, &once, log);

        assert!(
            !skipped,
            "non-rootless verdict must keep SQPOLL on its normal kernel path"
        );
        assert!(
            !once.is_completed(),
            "Once guard must stay armed so a later rootless verdict can still log once"
        );
    }

    #[test]
    fn build_ring_no_sqpoll_with_mmap_basis_no_warning() {
        // mmap_basis_active without sqpoll is a no-op: no warning, no
        // fallback flag, plain ring construction.
        SQPOLL_FALLBACK.store(false, Ordering::Relaxed);
        let config = IoUringConfig {
            sqpoll: false,
            mmap_basis_active: true,
            ..IoUringConfig::default()
        };
        let _ = config.build_ring();
        assert!(
            !sqpoll_fell_back(),
            "fallback flag must stay clear when SQPOLL was never requested"
        );
    }
}
