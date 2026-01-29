//! Example demonstrating rsync tracing integration.
//!
//! This example shows how to initialize and use the tracing system
//! with rsync's verbosity configuration.

use logging::{VerbosityConfig, init_tracing};

fn main() {
    // Initialize with verbosity level 2 (-vv)
    let config = VerbosityConfig::from_verbose_level(2);
    init_tracing(config);

    // Now use standard tracing macros
    tracing::info!(target: "rsync::copy", "Starting file transfer");
    tracing::debug!(target: "rsync::flist", "Building file list: {} entries", 42);
    tracing::debug!(target: "rsync::delta", "Computing delta for large_file.dat");
    tracing::trace!(target: "rsync::io", "Read 4096 bytes from fd 3");

    // Or use convenience macros
    logging::trace_copy!("Transferred file.txt ({} bytes)", 1234);
    logging::trace_stats!("Total: {} files, {} bytes", 10, 12345);
    logging::trace_proto!("Negotiated protocol version {}", 31);

    println!("\nCheck that events were recorded:");
    let events = logging::drain_events();
    println!("Captured {} diagnostic events", events.len());
}
