//! I/O policy enums.
//!
//! These enums let callers steer runtime selection of fast-path I/O
//! mechanisms (io_uring, IOCP, kernel zero-copy, copy-on-write reflink)
//! and map onto the corresponding CLI flags.

#[allow(unused_imports)]
use crate::platform_copy;

/// Policy controlling io_uring usage for file and socket I/O.
///
/// This enum allows callers to explicitly enable, disable, or auto-detect
/// io_uring support. It is used by CLI flags `--io-uring` and `--no-io-uring`.
///
/// # Runtime detection
///
/// When set to `Auto`, the runtime check ([`crate::io_uring::is_io_uring_available`])
/// performs three validations, caching the result in a process-wide atomic for
/// subsequent fast-path lookups:
///
/// 1. **Kernel version** - Parses `uname().release` and requires >= 5.6.
/// 2. **Syscall availability** - Attempts to create a minimal 4-entry io_uring
///    instance. This catches seccomp filters or container runtimes that block
///    `io_uring_setup(2)`.
/// 3. **Ring construction** - On first actual I/O, `IoUringConfig::build_ring`
///    creates the real ring. If SQPOLL is requested but the process lacks
///    `CAP_SYS_NICE`, it falls back to a normal ring silently.
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
    /// Auto-detect io_uring availability at runtime (default).
    ///
    /// Uses io_uring on Linux 5.6+ when the `io_uring` feature is enabled
    /// and the kernel supports it. Falls back to standard buffered I/O
    /// otherwise. This is the recommended setting for production use.
    #[default]
    Auto,
    /// Force io_uring usage. Returns an error if io_uring is unavailable.
    ///
    /// Useful for testing or when io_uring is required for performance
    /// guarantees. Fails with `ErrorKind::Unsupported` on non-Linux
    /// platforms, kernels older than 5.6, or when seccomp blocks the
    /// syscalls.
    Enabled,
    /// Disable io_uring; always use standard buffered I/O.
    ///
    /// Useful for benchmarking or diagnosing io_uring-related issues.
    Disabled,
}

/// Policy controlling copy-on-write reflink usage for whole-file copies.
///
/// This enum allows callers to disable CoW (`FICLONE`/`copy_file_range` on
/// Linux, `clonefile`/`fcopyfile` on macOS, `FSCTL_DUPLICATE_EXTENTS`/
/// `CopyFileExW` on Windows) and force the portable `std::fs::copy`
/// fallback. Useful for benchmarking, diagnostics, or when downstream
/// tooling does not handle reflinks correctly.
///
/// The `--cow` (default) and `--no-cow` CLI flags map onto this enum:
/// - `--cow` selects [`CowPolicy::Auto`].
/// - `--no-cow` selects [`CowPolicy::Disabled`].
///
/// The default is [`CowPolicy::Auto`], which delegates to
/// [`platform_copy::DefaultPlatformCopy`] and uses the best available reflink
/// mechanism with portable fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CowPolicy {
    /// Auto-detect reflink support and use it when available (default).
    ///
    /// Delegates to [`platform_copy::DefaultPlatformCopy`] which selects the best
    /// available copy mechanism per platform with portable fallback.
    #[default]
    Auto,
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
/// This enum allows callers to explicitly enable, disable, or auto-detect
/// IOCP support. It mirrors [`IoUringPolicy`] for the Windows platform.
///
/// # Runtime detection
///
/// When set to `Auto`, the runtime check ([`crate::iocp::is_iocp_available`])
/// creates a test completion port and caches the result. On Windows Vista+,
/// IOCP is always available. Files smaller than 64 KB use standard I/O
/// regardless of this policy since the async overhead exceeds the benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IocpPolicy {
    /// Auto-detect IOCP availability at runtime (default).
    ///
    /// Uses IOCP on Windows when the `iocp` feature is enabled.
    /// Falls back to standard buffered I/O otherwise.
    #[default]
    Auto,
    /// Force IOCP usage. Returns an error if IOCP is unavailable.
    ///
    /// Useful for testing or when IOCP is required for performance.
    /// Fails with `ErrorKind::Unsupported` on non-Windows platforms.
    Enabled,
    /// Disable IOCP; always use standard buffered I/O.
    ///
    /// Useful for benchmarking or diagnosing IOCP-related issues.
    Disabled,
}

/// Policy controlling I/O-level zero-copy syscalls (`sendfile`, `splice`,
/// `copy_file_range`, io_uring `IORING_OP_SEND_ZC`).
///
/// This enum gates kernel zero-copy data movement between file descriptors
/// and sockets. It is orthogonal to filesystem-level reflink/CoW cloning
/// (controlled by the separate cow policy). When [`ZeroCopyPolicy::Disabled`]
/// is in effect, callers route through standard userspace `read`/`write`
/// loops; the wrapped [`platform_copy::DefaultPlatformCopy`] strategy is
/// replaced by [`platform_copy::NoZeroCopyPlatformCopy`] which forces a
/// portable buffered copy.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZeroCopyPolicy {
    /// Auto-detect zero-copy availability at runtime (default).
    ///
    /// Uses `sendfile`, `splice`, `copy_file_range`, and io_uring `SEND_ZC`
    /// when the kernel supports them. Falls back to standard buffered I/O
    /// otherwise. This is the recommended setting for production use.
    #[default]
    Auto,
    /// Force zero-copy usage where supported.
    ///
    /// Useful for testing or benchmarking the zero-copy code paths.
    /// On platforms or filesystems that do not support a particular
    /// syscall, the call still falls back to standard I/O - this policy
    /// does not error, it simply opts in to the optimization where
    /// possible.
    Enabled,
    /// Disable zero-copy; always use standard buffered read/write.
    ///
    /// Useful for benchmarking, diagnosing zero-copy related issues,
    /// or working around kernels where `sendfile`/`splice` are blocked
    /// by seccomp filters. When set, callers route through portable
    /// userspace copy loops and io_uring socket sends fall back from
    /// `IORING_OP_SEND_ZC` to `IORING_OP_SEND`.
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::splice::{is_splice_available, is_splice_enabled};

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
}
