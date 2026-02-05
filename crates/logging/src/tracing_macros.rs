//! crates/logging/src/tracing_macros.rs
//! Convenience macros for rsync-specific tracing.
//!
//! These macros provide ergonomic wrappers around standard tracing macros
//! with appropriate targets for rsync subsystems.

/// Emit a copy operation trace.
///
/// # Example
/// ```ignore
/// trace_copy!("copying {}", path);
/// ```
#[macro_export]
macro_rules! trace_copy {
    ($($arg:tt)*) => {
        ::tracing::info!(target: "rsync::copy", $($arg)*);
    };
}

/// Emit a deletion operation trace.
///
/// # Example
/// ```ignore
/// trace_del!("deleting {}", path);
/// ```
#[macro_export]
macro_rules! trace_del {
    ($($arg:tt)*) => {
        ::tracing::info!(target: "rsync::delete", $($arg)*);
    };
}

/// Emit a file list trace.
///
/// # Example
/// ```ignore
/// trace_flist!("building file list: {} entries", count);
/// ```
#[macro_export]
macro_rules! trace_flist {
    ($($arg:tt)*) => {
        ::tracing::debug!(target: "rsync::flist", $($arg)*);
    };
}

/// Emit a statistics trace.
///
/// # Example
/// ```ignore
/// trace_stats!("transferred {} bytes", bytes);
/// ```
#[macro_export]
macro_rules! trace_stats {
    ($($arg:tt)*) => {
        ::tracing::info!(target: "rsync::stats", $($arg)*);
    };
}

/// Emit a protocol debug trace.
///
/// # Example
/// ```ignore
/// trace_proto!("negotiated protocol version {}", version);
/// ```
#[macro_export]
macro_rules! trace_proto {
    ($($arg:tt)*) => {
        ::tracing::debug!(target: "rsync::protocol", $($arg)*);
    };
}

/// Emit a delta computation trace.
///
/// # Example
/// ```ignore
/// trace_delta!("computed delta: {} blocks", count);
/// ```
#[macro_export]
macro_rules! trace_delta {
    ($($arg:tt)*) => {
        ::tracing::debug!(target: "rsync::delta", $($arg)*);
    };
}

/// Emit a receiver operation trace.
///
/// # Example
/// ```ignore
/// trace_recv!("received block offset={}", offset);
/// ```
#[macro_export]
macro_rules! trace_recv {
    ($($arg:tt)*) => {
        ::tracing::debug!(target: "rsync::receiver", $($arg)*);
    };
}

/// Emit a sender operation trace.
///
/// # Example
/// ```ignore
/// trace_send!("sending file {}", name);
/// ```
#[macro_export]
macro_rules! trace_send {
    ($($arg:tt)*) => {
        ::tracing::debug!(target: "rsync::sender", $($arg)*);
    };
}

/// Emit an I/O operation trace.
///
/// # Example
/// ```ignore
/// trace_io!("read {} bytes from {}", count, fd);
/// ```
#[macro_export]
macro_rules! trace_io {
    ($($arg:tt)*) => {
        ::tracing::trace!(target: "rsync::io", $($arg)*);
    };
}

/// Emit a connection trace.
///
/// # Example
/// ```ignore
/// trace_connect!("connecting to {}", host);
/// ```
#[macro_export]
macro_rules! trace_connect {
    ($($arg:tt)*) => {
        ::tracing::debug!(target: "rsync::connect", $($arg)*);
    };
}

/// Emit a filter rule trace.
///
/// # Example
/// ```ignore
/// trace_filter!("applied rule: {}", rule);
/// ```
#[macro_export]
macro_rules! trace_filter {
    ($($arg:tt)*) => {
        ::tracing::debug!(target: "rsync::filter", $($arg)*);
    };
}

/// Emit a generator operation trace.
///
/// # Example
/// ```ignore
/// trace_genr!("generating for {}", path);
/// ```
#[macro_export]
macro_rules! trace_genr {
    ($($arg:tt)*) => {
        ::tracing::debug!(target: "rsync::generator", $($arg)*);
    };
}
