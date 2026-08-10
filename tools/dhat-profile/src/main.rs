//! Dhat heap profiling harness for oc-rsync.
//!
//! This standalone tool profiles heap allocations using dhat.
//! The output can be analyzed with dhat-viewer.
//!
//! # Usage
//!
//! ```bash
//! # Build and run from the workspace root
//! cargo build --release --manifest-path tools/dhat-profile/Cargo.toml
//! cargo run --release --manifest-path tools/dhat-profile/Cargo.toml
//!
//! # Analyze with dhat-viewer
//! # Open https://nicholass.github.io/nicholasses-dhat-viewer/ and load dhat-heap.json
//! ```

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use std::env;
use std::fs;
use std::path::Path;

/// Creates test data for profiling.
fn setup_test_data(dir: &Path, file_count: usize, file_size: usize) {
    fs::create_dir_all(dir).expect("failed to create dir");
    for i in 0..file_count {
        let path = dir.join(format!("file_{i:04}.dat"));
        let data: Vec<u8> = (0..file_size).map(|j| ((i + j) % 256) as u8).collect();
        fs::write(&path, &data).expect("failed to write test file");
    }
}

fn main() {
    let _profiler = dhat::Profiler::new_heap();

    // Use temp directory based on env or /tmp
    let base = env::temp_dir();
    let workdir = base.join(format!("dhat_profile_{}", std::process::id()));
    let src_dir = workdir.join("src");
    let dest_dir = workdir.join("dest");

    // Setup test data: 100 files x 10KB each
    println!("Setting up test data: 100 files x 10KB");
    setup_test_data(&src_dir, 100, 10 * 1024);
    fs::create_dir_all(&dest_dir).expect("failed to create dest dir");

    println!("Source: {}", src_dir.display());
    println!("Dest: {}", dest_dir.display());

    // Note: In a real harness, invoke the rsync transfer logic here.
    // The actual profiling should call into core::client::run_transfer() or similar.

    println!("\nTo profile actual transfers, modify this harness to call:");
    println!("  core::client::run_local_copy() or");
    println!("  core::client::run_rsync_transfer()");
    println!("\nThe dhat-heap.json file will be written on exit.");

    // Simulate some allocations for demonstration
    let _data: Vec<Vec<u8>> = (0..1000).map(|i| vec![0u8; 1024 * (i % 10 + 1)]).collect();

    println!("\nProfiling complete. Check dhat-heap.json for results.");

    // Cleanup
    let _ = fs::remove_dir_all(&workdir);
}
