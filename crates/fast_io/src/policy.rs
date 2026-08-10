//! I/O policy enums.
//!
//! These enums let callers steer runtime selection of fast-path I/O
//! mechanisms (io_uring, IOCP, kernel zero-copy, copy-on-write reflink)
//! and map onto the corresponding CLI flags.
//!
//! [`IocpPolicy`] and [`ZeroCopyPolicy`] share the `Auto / Enabled /
//! Disabled` shape and are type aliases of the canonical [`BackendPolicy`]
//! enum, avoiding duplicated rustdoc, `Default`, and pattern-matching
//! boilerplate while preserving each name at the API surface.
//! [`IoUringPolicy`] is structurally similar but adds a fourth `SqpollOff`
//! arm for the rootless-container opt-out path, so it is defined as its own
//! enum rather than an alias. [`CowPolicy`] keeps a separate two-variant
//! definition because reflink is best-effort and has no `Enabled` semantics.

#[allow(unused_imports)]
use crate::platform_copy;

/// Three-way opt-in/opt-out policy shared by every fast-path I/O subsystem.
///
/// `Auto` lets the runtime pick the best mechanism the host supports;
/// `Enabled` forces the subsystem and errors out if it is unavailable;
/// `Disabled` always routes through the portable buffered fallback.
///
/// This is the canonical type used by [`IocpPolicy`] and [`ZeroCopyPolicy`].
/// Each alias preserves the original name for source compatibility and
/// CLI-flag wiring. [`IoUringPolicy`] is a distinct enum so it can expose a
/// fourth `SqpollOff` variant without polluting the shared shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendPolicy {
    /// Auto-detect availability at runtime (default).
    ///
    /// Uses the fast path when the host supports it and silently falls back
    /// to standard buffered I/O otherwise. This is the recommended setting
    /// for production use.
    #[default]
    Auto,
    /// Force the fast path. Returns an error if the subsystem is unavailable.
    ///
    /// Useful for testing or when the fast path is required for performance
    /// guarantees. Fails with `ErrorKind::Unsupported` on platforms or kernels
    /// that do not support the requested mechanism.
    Enabled,
    /// Disable the fast path; always use standard buffered I/O.
    ///
    /// Useful for benchmarking or diagnosing fast-path related issues.
    Disabled,
}

/// Policy controlling io_uring usage for file and socket I/O.
///
/// Steered by the CLI flags `--io-uring`, `--no-io-uring`, and
/// `--no-io-uring-sqpoll`. Mirrors [`BackendPolicy`] but adds a fourth
/// [`SqpollOff`](IoUringPolicy::SqpollOff) variant which keeps io_uring
/// active for file and socket I/O while suppressing the
/// `IORING_SETUP_SQPOLL` kernel-thread request. This is the explicit
/// opt-out for environments where SQPOLL cannot be granted
/// `CAP_SYS_NICE` (most notably rootless Kubernetes pods); operators
/// pick it to match production behaviour in non-K8s test environments
/// without disabling io_uring entirely.
///
/// # Runtime detection
///
/// When set to `Auto` or `SqpollOff`, the runtime check
/// ([`crate::io_uring::is_io_uring_available`]) performs three
/// validations, caching the result in a process-wide atomic for
/// subsequent fast-path lookups:
///
/// 1. **Kernel version** - Parses `uname().release` and requires >= 5.6.
/// 2. **Syscall availability** - Attempts to create a minimal 4-entry io_uring
///    instance. This catches seccomp filters or container runtimes that block
///    `io_uring_setup(2)`.
/// 3. **Ring construction** - On first actual I/O, `IoUringConfig::build_ring`
///    creates the real ring. If SQPOLL is requested but the process lacks
///    `CAP_SYS_NICE`, it falls back to a normal ring silently. With
///    `SqpollOff`, the SQPOLL request is skipped entirely.
///
/// If any step fails, the factory transparently returns a standard buffered
/// I/O reader or writer with no error.
///
/// # Kernel version requirements
///
/// | Feature | Minimum kernel | Notes |
/// |---------|---------------|-------|
/// | Basic io_uring (read/write) | 5.6 | `io_uring_setup`, `io_uring_enter` |
/// | `IORING_REGISTER_FILES` | 5.6 | Fixed-file descriptors, ~50ns/SQE savings |
/// | `IORING_SETUP_SQPOLL` | 5.6 | Kernel-side SQ polling, needs `CAP_SYS_NICE` |
/// | `IORING_OP_SEND` (socket I/O) | 5.6 | Used for socket writer batching |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IoUringPolicy {
    /// Auto-detect availability at runtime (default).
    ///
    /// Equivalent to [`BackendPolicy::Auto`]: probe the kernel, use
    /// io_uring when available, fall back to standard buffered I/O
    /// otherwise. SQPOLL is attempted when configured and transparently
    /// downgraded if `CAP_SYS_NICE` is missing.
    #[default]
    Auto,
    /// Force io_uring on. Returns an error if the kernel does not support it.
    ///
    /// Equivalent to [`BackendPolicy::Enabled`].
    Enabled,
    /// Disable io_uring entirely; always use standard buffered I/O.
    ///
    /// Equivalent to [`BackendPolicy::Disabled`].
    Disabled,
    /// Keep io_uring on but forbid `IORING_SETUP_SQPOLL`.
    ///
    /// io_uring still initialises and BGID, registered buffers, file
    /// registration, and every other feature remain active. Only the
    /// SQPOLL kernel-thread request is suppressed. Selected via
    /// `--no-io-uring-sqpoll`. The CLI parser also calls
    /// [`crate::io_uring::set_sqpoll_disabled_by_policy`] so that ring
    /// construction sites that internally request SQPOLL (e.g.
    /// dedicated session pools) honour the opt-out without each call
    /// site re-reading the policy.
    ///
    /// Use this in rootless containers and Kubernetes pods that
    /// cannot grant `CAP_SYS_NICE`: it gives operators an explicit
    /// guarantee that the SQPOLL kthread is never requested, instead
    /// of relying on the transparent `EPERM` fallback path. The
    /// difference is observable in `--io-uring-status` (no
    /// `sqpoll fell back: yes` line) and matters when audit policy
    /// disallows even unsuccessful SQPOLL setup attempts.
    SqpollOff,
}

impl IoUringPolicy {
    /// Returns `true` when io_uring should be active for file and socket I/O.
    ///
    /// This is `true` for [`Auto`](Self::Auto), [`Enabled`](Self::Enabled),
    /// and [`SqpollOff`](Self::SqpollOff); only [`Disabled`](Self::Disabled)
    /// routes through the portable buffered fallback. Used by dispatch sites
    /// that previously distinguished `Auto`/`Enabled` from `Disabled` and now
    /// also need to treat `SqpollOff` as an io_uring-active mode.
    #[must_use]
    pub fn is_io_uring_active(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Returns `true` when SQPOLL must be suppressed regardless of any other
    /// configuration that would otherwise request it.
    ///
    /// Only [`SqpollOff`](Self::SqpollOff) returns `true`. Wired into the CLI
    /// path so that ring construction sites can short-circuit SQPOLL via the
    /// process-global gate set by
    /// [`crate::io_uring::set_sqpoll_disabled_by_policy`].
    #[must_use]
    pub fn forbids_sqpoll(self) -> bool {
        matches!(self, Self::SqpollOff)
    }
}

/// Policy controlling copy-on-write reflink usage for whole-file copies.
///
/// This enum allows callers to disable CoW (`FICLONE`/`copy_file_range` on
/// Linux, `clonefile`/`fcopyfile` on macOS, `FSCTL_DUPLICATE_EXTENTS`/
/// `CopyFileExW` on Windows) and force the portable `std::fs::copy`
/// fallback. Useful for benchmarking, diagnostics, or when downstream
/// tooling does not handle reflinks correctly.
///
/// Two CLI surfaces drive this enum:
///
/// - `--cow` / `--no-cow` is the binary opt-in/opt-out:
///   - `--cow` selects [`CowPolicy::Auto`].
///   - `--no-cow` selects [`CowPolicy::Disabled`].
/// - `--reflink=<MODE>` is the tri-state form:
///   - `--reflink=auto` selects [`CowPolicy::Auto`].
///   - `--reflink=always` selects [`CowPolicy::Required`].
///   - `--reflink=never` selects [`CowPolicy::Disabled`].
///
/// The default is [`CowPolicy::Auto`], which delegates to
/// [`platform_copy::DefaultPlatformCopy`] and uses the best available reflink
/// mechanism with portable fallback.
///
/// [`CowPolicy::Required`] forces every whole-file copy through a CoW
/// reflink and surfaces the underlying error when the destination
/// filesystem does not support it (no silent fallback). Use it when a
/// downstream guarantee depends on block sharing (snapshot dedup,
/// container layer builds) and a portable `std::fs::copy` fallback would
/// silently violate that guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CowPolicy {
    /// Auto-detect reflink support and use it when available (default).
    ///
    /// Delegates to [`platform_copy::DefaultPlatformCopy`] which selects the best
    /// available copy mechanism per platform with portable fallback.
    #[default]
    Auto,
    /// Require copy-on-write reflinks; fail when the destination
    /// filesystem cannot honour the request.
    ///
    /// Selected by `--reflink=always`. Mirrors `BackendPolicy::Enabled`
    /// semantics: the copy returns an error instead of falling back to
    /// the portable `std::fs::copy` path when the platform reflink
    /// attempt reports `ErrorKind::Unsupported` (or any other failure).
    Required,
    /// Disable copy-on-write reflinks; always use portable `std::fs::copy`.
    ///
    /// Forces every whole-file copy through the standard buffered fallback,
    /// bypassing `FICLONE`, `copy_file_range`, `clonefile`, `fcopyfile`,
    /// `FSCTL_DUPLICATE_EXTENTS`, and `CopyFileExW`. Useful when destination
    /// filesystems mishandle reflinks or for measuring CoW performance gains.
    Disabled,
}

/// Policy controlling IOCP usage for file I/O on Windows.
///
/// Alias of [`BackendPolicy`]. Mirrors [`IoUringPolicy`] for the Windows
/// platform.
///
/// # Runtime detection
///
/// When set to `Auto`, the runtime check ([`crate::iocp::is_iocp_available`])
/// creates a test completion port and caches the result. On Windows Vista+,
/// IOCP is always available. Files smaller than 64 KB use standard I/O
/// regardless of this policy since the async overhead exceeds the benefit.
pub type IocpPolicy = BackendPolicy;

/// Policy controlling I/O-level zero-copy syscalls (`sendfile`, `splice`,
/// `copy_file_range`, io_uring `IORING_OP_SEND_ZC`).
///
/// Alias of [`BackendPolicy`]. Gates kernel zero-copy data movement between
/// file descriptors and sockets. Orthogonal to filesystem-level reflink/CoW
/// cloning (controlled by the separate [`CowPolicy`]). When
/// [`ZeroCopyPolicy::Disabled`] is in effect, callers route through standard
/// userspace `read`/`write` loops; the wrapped
/// [`platform_copy::DefaultPlatformCopy`] strategy is replaced by
/// [`platform_copy::NoZeroCopyPlatformCopy`] which forces a portable buffered
/// copy.
///
/// # Precedence with the cow policy
///
/// `--cow` controls FS-level extent sharing (`FICLONE`, `clonefile`, ReFS
/// `FSCTL_DUPLICATE_EXTENTS_TO_FILE`). `--zero-copy` controls IO-level
/// kernel-side data movement (`sendfile`, `splice`, `copy_file_range`,
/// `SEND_ZC`). The two are independent: a transfer can use reflink without
/// `sendfile` (whole-file CoW clone) or `sendfile` without reflink (network
/// send of a file). Disabling either does not affect the other.
///
/// # Runtime fallback chain
///
/// When set to [`ZeroCopyPolicy::Auto`], the platform fallback chain in
/// [`platform_copy::DefaultPlatformCopy`] selects the best mechanism. When
/// set to [`ZeroCopyPolicy::Disabled`], the chain is bypassed and callers
/// use [`std::fs::copy`] (which still uses kernel optimizations on some
/// platforms but skips `copy_file_range`/`sendfile`/`splice` direct calls).
pub type ZeroCopyPolicy = BackendPolicy;

/// Size threshold (bytes) above which basis-file reads use io_uring+SQPOLL
/// instead of mmap.
///
/// Below this size, mmap setup cost outweighs the per-batch
/// `io_uring_enter(2)` syscall savings. Above it, the syscall-amortisation
/// win of SQPOLL+`READ_FIXED` dominates. The default value (64 KiB) comes
/// from the SMR-2 decision matrix in
/// `docs/design/mmap-vs-sqpoll-conflict-resolution.md` as the most
/// conservative cut-over point that still preserves the mmap fast-path for
/// the very small basis files where neither backend has a clear win.
///
/// Operators can override this at runtime via the
/// `OC_RSYNC_MMAP_TO_SQPOLL_THRESHOLD_BYTES` environment variable; see
/// [`mmap_to_sqpoll_threshold_bytes`].
pub const MMAP_TO_SQPOLL_THRESHOLD: u64 = 64 * 1024;

/// Environment variable that overrides [`MMAP_TO_SQPOLL_THRESHOLD`].
///
/// Parsed once per process with [`std::sync::OnceLock`]; later changes are
/// ignored. The value must be a base-10 unsigned 64-bit integer in bytes.
/// Malformed or empty values fall back to the compile-time default.
pub const MMAP_TO_SQPOLL_THRESHOLD_ENV: &str = "OC_RSYNC_MMAP_TO_SQPOLL_THRESHOLD_BYTES";

/// Backend selected for a single basis-file read by
/// [`choose_basis_read_backend`].
///
/// Encodes the SMR-2 decision framework: mmap is preferred only when the
/// basis file is small enough that the mmap setup cost outweighs the
/// io_uring submission overhead; otherwise io_uring is preferred when
/// available, and a portable `BufReader<File>` is the universal fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasisReadBackend {
    /// Memory-mapped read path. Selected when the basis file size is
    /// strictly below the [`MMAP_TO_SQPOLL_THRESHOLD`] cut-over and the
    /// mmap path is available on the host.
    Mmap,
    /// io_uring `READ_FIXED` path with SQPOLL. Selected when the basis
    /// file is at or above the cut-over and the platform supports the
    /// data-reads code path.
    IoUring,
    /// Portable `BufReader<File>` fallback. Selected when neither the
    /// mmap nor the io_uring path is available (non-Unix host, missing
    /// kernel feature, disabled cargo feature, etc.).
    BufReader,
}

/// Returns the runtime threshold (bytes) for the mmap-to-io_uring cut-over.
///
/// First call parses [`MMAP_TO_SQPOLL_THRESHOLD_ENV`] and caches the result
/// in a process-wide [`std::sync::OnceLock`]. Later calls return the cached
/// value. Empty or malformed environment values fall back to
/// [`MMAP_TO_SQPOLL_THRESHOLD`].
///
/// The override exists so operators can tune the cut-over per host after
/// running the bench harness in `crates/fast_io/benches/`. The default is
/// deliberately conservative; raising it pushes more workload onto mmap,
/// lowering it pushes more onto io_uring.
#[must_use]
pub fn mmap_to_sqpoll_threshold_bytes() -> u64 {
    use std::sync::OnceLock;

    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| {
        resolve_mmap_to_sqpoll_threshold(std::env::var(MMAP_TO_SQPOLL_THRESHOLD_ENV).ok())
    })
}

/// Pure resolver shared by [`mmap_to_sqpoll_threshold_bytes`] and unit
/// tests. Extracted so the env-var parsing logic is testable without
/// touching the process-wide `OnceLock` cache.
#[must_use]
fn resolve_mmap_to_sqpoll_threshold(raw: Option<String>) -> u64 {
    raw.as_deref()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(MMAP_TO_SQPOLL_THRESHOLD)
}

/// Selects the backend for a single basis-file read using the SMR-2
/// size-threshold heuristic, against an explicit threshold value.
///
/// Decision rules, evaluated in order:
///
/// 1. If `file_size_bytes` is strictly below `threshold_bytes` **and**
///    `mmap_available` is true, return [`BasisReadBackend::Mmap`].
/// 2. Otherwise, if `iouring_available` is true, return
///    [`BasisReadBackend::IoUring`].
/// 3. Otherwise, return [`BasisReadBackend::BufReader`].
///
/// Production callers should prefer [`choose_basis_read_backend`], which
/// supplies `threshold_bytes` from [`mmap_to_sqpoll_threshold_bytes`].
/// This explicit form exists for tests and for callers that have already
/// resolved a per-transfer threshold override.
#[must_use]
pub fn choose_basis_read_backend_with_threshold(
    file_size_bytes: u64,
    mmap_available: bool,
    iouring_available: bool,
    threshold_bytes: u64,
) -> BasisReadBackend {
    if mmap_available && file_size_bytes < threshold_bytes {
        return BasisReadBackend::Mmap;
    }
    if iouring_available {
        return BasisReadBackend::IoUring;
    }
    BasisReadBackend::BufReader
}

/// Selects the backend for a single basis-file read using the SMR-2
/// size-threshold heuristic.
///
/// Reads the runtime threshold from [`mmap_to_sqpoll_threshold_bytes`],
/// which honours the `OC_RSYNC_MMAP_TO_SQPOLL_THRESHOLD_BYTES` env var.
/// See [`choose_basis_read_backend_with_threshold`] for the decision
/// rules.
///
/// Callers are responsible for passing the correct availability flags -
/// typically the result of [`crate::io_uring::is_io_uring_available`]
/// (or a feature-gated probe) for `iouring_available`, and an equivalent
/// platform check for `mmap_available`.
#[must_use]
pub fn choose_basis_read_backend(
    file_size_bytes: u64,
    mmap_available: bool,
    iouring_available: bool,
) -> BasisReadBackend {
    choose_basis_read_backend_with_threshold(
        file_size_bytes,
        mmap_available,
        iouring_available,
        mmap_to_sqpoll_threshold_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::splice::{is_splice_available, is_splice_enabled};

    #[test]
    fn backend_policy_default_is_auto() {
        assert_eq!(BackendPolicy::default(), BackendPolicy::Auto);
    }

    #[test]
    fn backend_policy_variants_are_distinct() {
        assert_ne!(BackendPolicy::Auto, BackendPolicy::Enabled);
        assert_ne!(BackendPolicy::Auto, BackendPolicy::Disabled);
        assert_ne!(BackendPolicy::Enabled, BackendPolicy::Disabled);
    }

    #[test]
    fn aliases_resolve_to_backend_policy() {
        let b: IocpPolicy = IocpPolicy::Auto;
        let c: ZeroCopyPolicy = ZeroCopyPolicy::Auto;
        assert_eq!(b, c);
        assert_eq!(c, BackendPolicy::Auto);
    }

    #[test]
    fn io_uring_policy_default_is_auto() {
        assert_eq!(IoUringPolicy::default(), IoUringPolicy::Auto);
    }

    #[test]
    fn io_uring_policy_four_variants_are_distinct() {
        let variants = [
            IoUringPolicy::Auto,
            IoUringPolicy::Enabled,
            IoUringPolicy::Disabled,
            IoUringPolicy::SqpollOff,
        ];
        for (i, a) in variants.iter().enumerate() {
            for b in &variants[i + 1..] {
                assert_ne!(a, b, "{a:?} must differ from {b:?}");
            }
        }
    }

    #[test]
    fn io_uring_policy_sqpoll_off_keeps_io_uring_active() {
        assert!(IoUringPolicy::SqpollOff.is_io_uring_active());
        assert!(IoUringPolicy::Auto.is_io_uring_active());
        assert!(IoUringPolicy::Enabled.is_io_uring_active());
        assert!(!IoUringPolicy::Disabled.is_io_uring_active());
    }

    #[test]
    fn io_uring_policy_only_sqpoll_off_forbids_sqpoll() {
        assert!(IoUringPolicy::SqpollOff.forbids_sqpoll());
        assert!(!IoUringPolicy::Auto.forbids_sqpoll());
        assert!(!IoUringPolicy::Enabled.forbids_sqpoll());
        assert!(!IoUringPolicy::Disabled.forbids_sqpoll());
    }

    #[test]
    fn cow_policy_default_is_auto() {
        assert_eq!(CowPolicy::default(), CowPolicy::Auto);
    }

    #[test]
    fn cow_policy_variants_are_distinct() {
        let variants = [CowPolicy::Auto, CowPolicy::Required, CowPolicy::Disabled];
        for (i, a) in variants.iter().enumerate() {
            for b in &variants[i + 1..] {
                assert_ne!(a, b, "{a:?} must differ from {b:?}");
            }
        }
    }

    #[test]
    fn zero_copy_policy_default_is_auto() {
        assert_eq!(ZeroCopyPolicy::default(), ZeroCopyPolicy::Auto);
    }

    #[test]
    fn zero_copy_policy_variants_are_distinct() {
        assert_ne!(ZeroCopyPolicy::Auto, ZeroCopyPolicy::Enabled);
        assert_ne!(ZeroCopyPolicy::Auto, ZeroCopyPolicy::Disabled);
        assert_ne!(ZeroCopyPolicy::Enabled, ZeroCopyPolicy::Disabled);
    }

    #[test]
    fn is_splice_enabled_respects_disabled_policy() {
        assert!(!is_splice_enabled(ZeroCopyPolicy::Disabled));
    }

    #[test]
    fn is_splice_enabled_auto_matches_availability() {
        assert_eq!(
            is_splice_enabled(ZeroCopyPolicy::Auto),
            is_splice_available()
        );
    }

    /// The conservative SMR-2 default must stay at 64 KiB until the bench
    /// harness produces hardware evidence that justifies moving it.
    #[test]
    fn mmap_to_sqpoll_threshold_default_is_64_kib() {
        assert_eq!(MMAP_TO_SQPOLL_THRESHOLD, 64 * 1024);
    }

    /// Files strictly below the cut-over must route to mmap when the mmap
    /// path is available, per the SMR-2 decision matrix.
    #[test]
    fn dispatch_uses_mmap_below_threshold() {
        let one_kib = 1024_u64;
        assert!(one_kib < MMAP_TO_SQPOLL_THRESHOLD);

        // mmap available + io_uring available: mmap wins under the threshold.
        assert_eq!(
            choose_basis_read_backend(one_kib, true, true),
            BasisReadBackend::Mmap,
        );
        // mmap available + io_uring absent: mmap still wins.
        assert_eq!(
            choose_basis_read_backend(one_kib, true, false),
            BasisReadBackend::Mmap,
        );
    }

    /// Files at or above the cut-over must route to io_uring when the
    /// io_uring data-reads path is available on the host.
    #[test]
    fn dispatch_uses_iouring_above_threshold() {
        let one_mib = 1024_u64 * 1024;
        assert!(one_mib >= MMAP_TO_SQPOLL_THRESHOLD);

        // mmap + io_uring both available: io_uring wins above the threshold.
        assert_eq!(
            choose_basis_read_backend(one_mib, true, true),
            BasisReadBackend::IoUring,
        );
        // Exactly at the cut-over also routes to io_uring (strict-below mmap).
        assert_eq!(
            choose_basis_read_backend(MMAP_TO_SQPOLL_THRESHOLD, true, true),
            BasisReadBackend::IoUring,
        );
    }

    /// When neither mmap nor io_uring is available, the dispatch must
    /// fall back to the portable `BufReader<File>` path.
    #[test]
    fn dispatch_falls_back_to_bufreader_when_no_backend_available() {
        let one_mib = 1024_u64 * 1024;
        assert_eq!(
            choose_basis_read_backend(one_mib, false, false),
            BasisReadBackend::BufReader,
        );
        assert_eq!(
            choose_basis_read_backend(1024, false, false),
            BasisReadBackend::BufReader,
        );
    }

    /// The `OC_RSYNC_MMAP_TO_SQPOLL_THRESHOLD_BYTES` env var, when set,
    /// must override the compile-time default both in the parsed
    /// threshold and in the resulting dispatch decision.
    ///
    /// Tests the resolver and the explicit-threshold dispatch directly
    /// instead of mutating the process environment, so it does not race
    /// the `OnceLock` cache shared by every other test in the binary.
    #[test]
    fn env_var_override_changes_threshold() {
        // Empty / absent env var falls back to the compile-time default.
        assert_eq!(
            resolve_mmap_to_sqpoll_threshold(None),
            MMAP_TO_SQPOLL_THRESHOLD,
        );
        assert_eq!(
            resolve_mmap_to_sqpoll_threshold(Some(String::new())),
            MMAP_TO_SQPOLL_THRESHOLD,
        );
        // Whitespace-only and malformed inputs also fall back.
        assert_eq!(
            resolve_mmap_to_sqpoll_threshold(Some("   ".to_string())),
            MMAP_TO_SQPOLL_THRESHOLD,
        );
        assert_eq!(
            resolve_mmap_to_sqpoll_threshold(Some("not-a-number".to_string())),
            MMAP_TO_SQPOLL_THRESHOLD,
        );

        // Valid overrides parse to the requested byte count (whitespace
        // around the number is tolerated).
        let override_bytes: u64 = 4096;
        assert_ne!(override_bytes, MMAP_TO_SQPOLL_THRESHOLD);
        assert_eq!(
            resolve_mmap_to_sqpoll_threshold(Some(override_bytes.to_string())),
            override_bytes,
        );
        assert_eq!(
            resolve_mmap_to_sqpoll_threshold(Some(format!("  {override_bytes}  "))),
            override_bytes,
        );

        // Dispatch decision honours the overridden threshold. A file
        // sized between the override (4 KiB) and the default (64 KiB)
        // routes to io_uring when the override is in effect.
        let between = override_bytes + 1;
        assert!(between < MMAP_TO_SQPOLL_THRESHOLD);
        assert_eq!(
            choose_basis_read_backend_with_threshold(between, true, true, override_bytes),
            BasisReadBackend::IoUring,
        );
        // A file strictly below the override still routes to mmap.
        assert_eq!(
            choose_basis_read_backend_with_threshold(
                override_bytes - 1,
                true,
                true,
                override_bytes,
            ),
            BasisReadBackend::Mmap,
        );
    }
}
