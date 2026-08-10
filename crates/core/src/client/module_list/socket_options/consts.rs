//! Platform-specific socket constants.
//!
//! On Unix, forwards directly to `libc`.
//! On Windows, provides Winsock-compatible numeric values.
//!
//! This is a small adapter so the rest of the module stays platform-neutral.

// RFC 1349 class selectors - consistent across Unix targets, but libc does not
// expose them uniformly (e.g. Apple platforms omit the aliases).
#[cfg(not(target_family = "windows"))]
pub(super) const IPTOS_LOWDELAY: libc::c_int = 0x10;

#[cfg(not(target_family = "windows"))]
pub(super) const IPTOS_THROUGHPUT: libc::c_int = 0x08;

/// Socket-level option protocol number.
///
/// Used as the `level` argument to `setsockopt()` for socket-level options
/// like `SO_KEEPALIVE`, `SO_REUSEADDR`, and buffer sizes.
///
/// # Platform values
/// - Unix: Typically `1` (from `libc::SOL_SOCKET`)
/// - Windows: `0xFFFF` (Winsock `SOL_SOCKET`)
#[cfg(not(target_family = "windows"))]
pub const SOL_SOCKET: libc::c_int = libc::SOL_SOCKET;

/// Socket-level option protocol number (Windows variant).
#[cfg(target_family = "windows")]
pub const SOL_SOCKET: libc::c_int = 0xFFFF;

/// Enable TCP keepalive probes on the connection.
///
/// When enabled, the kernel periodically sends keepalive probes on idle
/// connections to detect dead peers. Essential for long-running rsync
/// transfers over unreliable networks.
///
/// # Upstream rsync
/// Equivalent to `--sockopts=SO_KEEPALIVE`.
#[cfg(not(target_family = "windows"))]
pub const SO_KEEPALIVE: libc::c_int = libc::SO_KEEPALIVE;

/// Enable TCP keepalive probes (Windows variant).
#[cfg(target_family = "windows")]
pub const SO_KEEPALIVE: libc::c_int = 0x0008;

/// Allow reuse of local addresses in TIME_WAIT state.
///
/// Useful for rsync daemons that need to restart quickly without waiting
/// for socket cleanup.
///
/// # Upstream rsync
/// Equivalent to `--sockopts=SO_REUSEADDR`.
#[cfg(not(target_family = "windows"))]
pub const SO_REUSEADDR: libc::c_int = libc::SO_REUSEADDR;

/// Allow reuse of local addresses (Windows variant).
#[cfg(target_family = "windows")]
pub const SO_REUSEADDR: libc::c_int = 0x0004;

/// Allow sending broadcast messages on the socket.
///
/// # Upstream rsync
/// Equivalent to `--sockopts=SO_BROADCAST`.
#[cfg(not(target_family = "windows"))]
pub const SO_BROADCAST: libc::c_int = libc::SO_BROADCAST;

/// Allow sending broadcast messages (Windows variant).
#[cfg(target_family = "windows")]
pub const SO_BROADCAST: libc::c_int = 0x0020;

/// Set the send buffer size in bytes.
///
/// Larger buffers can improve throughput on high-latency or high-bandwidth
/// networks.
///
/// # Upstream rsync
/// Equivalent to `--sockopts=SO_SNDBUF=<n>`.
#[cfg(not(target_family = "windows"))]
pub const SO_SNDBUF: libc::c_int = libc::SO_SNDBUF;

/// Set the send buffer size (Windows variant).
#[cfg(target_family = "windows")]
pub const SO_SNDBUF: libc::c_int = 0x1001;

/// Set the receive buffer size in bytes.
///
/// Should be tuned alongside `SO_SNDBUF` for optimal performance.
///
/// # Upstream rsync
/// Equivalent to `--sockopts=SO_RCVBUF=<n>`.
#[cfg(not(target_family = "windows"))]
pub const SO_RCVBUF: libc::c_int = libc::SO_RCVBUF;

/// Set the receive buffer size (Windows variant).
#[cfg(target_family = "windows")]
pub const SO_RCVBUF: libc::c_int = 0x1002;

/// Set the send timeout in seconds.
///
/// Prevents rsync from hanging indefinitely on unresponsive connections.
///
/// # Upstream rsync
/// Equivalent to `--sockopts=SO_SNDTIMEO=<n>`.
#[cfg(not(target_family = "windows"))]
pub const SO_SNDTIMEO: libc::c_int = libc::SO_SNDTIMEO;

/// Set the send timeout (Windows variant).
#[cfg(target_family = "windows")]
pub const SO_SNDTIMEO: libc::c_int = 0x1005;

/// Set the receive timeout in seconds.
///
/// Prevents rsync from hanging indefinitely waiting for data from
/// unresponsive servers.
///
/// # Upstream rsync
/// Equivalent to `--sockopts=SO_RCVTIMEO=<n>`.
#[cfg(not(target_family = "windows"))]
pub const SO_RCVTIMEO: libc::c_int = libc::SO_RCVTIMEO;

/// Set the receive timeout (Windows variant).
#[cfg(target_family = "windows")]
pub const SO_RCVTIMEO: libc::c_int = 0x1006;

/// TCP protocol number for protocol-specific socket options.
///
/// Used as the `level` argument to `setsockopt()` for TCP-specific options
/// like `TCP_NODELAY`.
#[cfg(not(target_family = "windows"))]
pub const IPPROTO_TCP: libc::c_int = libc::IPPROTO_TCP;

/// TCP protocol number (Windows variant).
#[cfg(target_family = "windows")]
pub const IPPROTO_TCP: libc::c_int = 6;

/// Disable Nagle's algorithm for latency-sensitive transfers.
///
/// Reduces latency for interactive rsync sessions or when transferring
/// many small files, at the cost of potentially increased packet overhead.
///
/// # Upstream rsync
/// Equivalent to `--sockopts=TCP_NODELAY`. Often combined with
/// `IPTOS_LOWDELAY` for latency-sensitive workloads.
#[cfg(not(target_family = "windows"))]
pub const TCP_NODELAY: libc::c_int = libc::TCP_NODELAY;

/// Disable Nagle's algorithm (Windows variant).
#[cfg(target_family = "windows")]
pub const TCP_NODELAY: libc::c_int = 0x0001;
