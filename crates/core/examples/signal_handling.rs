//! Example demonstrating signal handling for graceful shutdown.
//!
//! This example shows how to:
//! 1. Install signal handlers at application startup
//! 2. Register temporary files for cleanup
//! 3. Check for shutdown requests during operations
//! 4. Clean up resources on shutdown
//!
//! Run this example with:
//! ```bash
//! cargo run --package core --example signal_handling
//! ```
//!
//! Then press Ctrl+C to trigger graceful shutdown, or press it twice
//! to force immediate termination.

use core::signal::CleanupManager;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Signal Handling Example");
    println!("=======================\n");

    // 1. Install signal handlers at program startup
    println!("Installing signal handlers...");
    let handler = core::signal::install_signal_handlers()?;
    println!("Signal handlers installed successfully\n");

    // 2. Create some temporary files to simulate work
    let temp_dir = tempfile::tempdir()?;
    let temp_files: Vec<PathBuf> = (0..5)
        .map(|i| {
            let path = temp_dir.path().join(format!("transfer_{i}.tmp"));
            println!("Creating temp file: {}", path.display());
            fs::write(&path, format!("Transfer data {i}")).expect("write file");
            path
        })
        .collect();

    // 3. Register temp files with cleanup manager
    println!("\nRegistering temp files for cleanup...");
    let manager = CleanupManager::global();
    for path in &temp_files {
        manager.register_temp_file(path.clone());
    }
    println!("Registered {} temp files", temp_files.len());

    // 4. Register a cleanup callback
    manager.register_cleanup(Box::new(|| {
        println!("Running cleanup callback: reporting statistics");
    }));

    // 5. Simulate a long-running operation with shutdown checks
    println!("\nStarting simulated file transfer...");
    println!("Press Ctrl+C once for graceful shutdown");
    println!("Press Ctrl+C twice for immediate termination\n");

    for i in 0..10 {
        // Check for shutdown request before processing each file
        if handler.is_shutdown_requested() {
            println!("\n*** Shutdown requested ***");
            if let Some(reason) = handler.shutdown_reason() {
                println!("Reason: {}", reason);
                println!("Exit code: {}", reason.exit_code().as_i32());
            }

            // Check if we should abort immediately
            if handler.is_abort_requested() {
                println!("*** Abort requested - terminating immediately ***");
                manager.cleanup();
                std::process::exit(core::exit_code::ExitCode::Signal.as_i32());
            }

            println!("Finishing current file before shutdown...");
            // Allow current file to complete
            thread::sleep(Duration::from_millis(500));
            println!("Current file completed");
            break;
        }

        // Simulate file processing
        println!("Processing file {}/10...", i + 1);
        thread::sleep(Duration::from_secs(2));

        // Simulate successful completion - unregister the temp file
        if i < temp_files.len() {
            println!("  File {} completed successfully", i + 1);
            manager.unregister_temp_file(&temp_files[i]);
        }
    }

    // 6. Clean up remaining temp files
    println!("\nPerforming final cleanup...");
    manager.cleanup();

    // 7. Determine exit code based on shutdown reason
    let exit_code = if let Some(reason) = handler.shutdown_reason() {
        println!("Exiting with code: {} ({})",
                 reason.exit_code().as_i32(),
                 reason.exit_code().description());
        reason.exit_code()
    } else {
        println!("Transfer completed successfully!");
        core::exit_code::ExitCode::Ok
    };

    std::process::exit(exit_code.as_i32())
}
