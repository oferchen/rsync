//! Benchmark comparing sequential vs batched metadata fetching.
//!
//! This example demonstrates the performance improvement from batching
//! stat operations during directory traversal.
//!
//! Usage:
//!   cargo run --release --features parallel --example batched_stat_benchmark -- /path/to/large/directory

use std::env;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <directory>", args[0]);
        eprintln!("Example: {} /usr/share", args[0]);
        std::process::exit(1);
    }

    let path = &args[1];
    println!("Benchmarking directory: {path}");
    println!();

    // Benchmark sequential traversal
    println!("=== Sequential Traversal ===");
    let start = Instant::now();
    let walker = flist::FileListBuilder::new(path).build()?;
    let mut count = 0;
    for entry in walker {
        let _ = entry?;
        count += 1;
    }
    let sequential_duration = start.elapsed();
    println!("Found {count} entries");
    println!("Time: {sequential_duration:?}");
    println!();

    #[cfg(feature = "parallel")]
    {
        // Benchmark parallel metadata fetching
        println!("=== Parallel with Batched Stats ===");
        let start = Instant::now();
        let result =
            flist::parallel::collect_with_batched_stats(std::path::PathBuf::from(path), false);

        let entries = match result {
            Ok(entries) => entries,
            Err(errors) => {
                eprintln!("Errors during parallel collection:");
                for (path, error) in errors.iter().take(5) {
                    eprintln!("  {}: {}", path.display(), error);
                }
                if errors.len() > 5 {
                    eprintln!("  ... and {} more errors", errors.len() - 5);
                }
                return Err("Failed to collect entries".into());
            }
        };

        let parallel_duration = start.elapsed();
        println!("Found {} entries", entries.len());
        println!("Time: {parallel_duration:?}");
        println!();

        // Calculate speedup
        let speedup = sequential_duration.as_secs_f64() / parallel_duration.as_secs_f64();
        println!("=== Results ===");
        println!("Speedup: {speedup:.2}x");
        println!("Time saved: {:?}", sequential_duration - parallel_duration);
    }

    #[cfg(not(feature = "parallel"))]
    {
        println!("Note: Compile with --features parallel to enable batched stats benchmark");
    }

    Ok(())
}
