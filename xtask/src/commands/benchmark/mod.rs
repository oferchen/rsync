//! Performance benchmarking command for comparing oc-rsync versions.
//!
//! This module provides the `benchmark` xtask command which compares performance
//! between upstream rsync, recent oc-rsync releases, and the current development
//! snapshot.
//!
//! Supports three modes:
//! - **Local**: Uses a local rsync daemon with Linux kernel source
//! - **Remote**: Tests against public rsync:// mirrors
//! - **Loopback**: Deterministic local benchmarks with per-version daemons

mod config;
mod loopback;
mod report;
mod runner;

#[cfg(test)]
mod tests;

pub use config::BenchmarkOptions;

use crate::cli::BenchmarkMode;
use crate::error::{TaskError, TaskResult};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use config::{REMOTE_MIRRORS, list_mirrors, url_short_name};
use loopback::run_loopback_benchmarks;
use report::{BenchmarkResultSet, output_json_multi, output_table_multi};
use runner::{
    BenchmarkResult, build_release, build_version, check_daemon_running, detect_versions,
    find_upstream_rsync, run_benchmark, start_daemon,
};

/// Executes the benchmark command.
pub fn execute(workspace: &Path, options: BenchmarkOptions) -> TaskResult<()> {
    if options.list_mirrors {
        return list_mirrors();
    }

    println!("=== oc-rsync Performance Benchmark ===\n");

    fs::create_dir_all(&options.bench_dir).map_err(|e| {
        TaskError::Validation(format!(
            "failed to create benchmark directory {:?}: {}",
            options.bench_dir, e
        ))
    })?;

    // Loopback mode runs its own orchestration with per-version daemons.
    if options.mode == BenchmarkMode::Loopback {
        return run_loopback_benchmarks(workspace, &options);
    }

    let urls = match options.mode {
        BenchmarkMode::Local => {
            if !check_daemon_running(options.port) {
                println!("Starting rsync daemon on port {}...", options.port);
                start_daemon(workspace, &options)?;
            } else {
                println!("Using existing rsync daemon on port {}", options.port);
            }
            vec![format!(
                "rsync://localhost:{}/kernel/arch/x86/",
                options.port
            )]
        }
        BenchmarkMode::Remote => {
            if options.urls.is_empty() {
                REMOTE_MIRRORS.iter().map(|m| m.url.to_string()).collect()
            } else {
                options.urls.clone()
            }
        }
        BenchmarkMode::Loopback => unreachable!("handled above"),
    };

    println!("Mode: {:?}", options.mode);
    println!("URLs to benchmark:");
    for url in &urls {
        println!("  - {url}");
    }
    println!();

    let versions = if options.versions.is_empty() {
        detect_versions(workspace)?
    } else {
        options.versions.clone()
    };

    println!("Versions to benchmark: {versions:?}\n");

    let mut binaries: HashMap<String, PathBuf> = HashMap::new();

    if let Some(rsync_path) = find_upstream_rsync() {
        binaries.insert("upstream".to_string(), rsync_path);
    }

    if !options.skip_build {
        println!("Building current development version...");
        build_release(workspace)?;
    }
    let current_binary = workspace.join("target/release/oc-rsync");
    if current_binary.exists() {
        binaries.insert("dev".to_string(), current_binary);
    }

    for version in &versions {
        if version == "dev" || version == "upstream" {
            continue;
        }
        let binary_path = options.bench_dir.join(format!("oc-rsync-{version}"));
        if !binary_path.exists() && !options.skip_build {
            println!("Building version {version}...");
            if let Err(e) = build_version(workspace, version, &binary_path) {
                eprintln!("Warning: failed to build {version}: {e}");
                continue;
            }
        }
        if binary_path.exists() {
            binaries.insert(version.clone(), binary_path);
        }
    }

    let dest_dir = options.bench_dir.join("bench-dest");
    let mut all_results: Vec<BenchmarkResultSet> = Vec::new();

    for url in &urls {
        let url_name = url_short_name(url);
        println!("\n=== Benchmarking against: {url_name} ({url}) ===");

        let mut url_results: Vec<BenchmarkResult> = Vec::new();

        for (version, binary) in &binaries {
            println!("\nBenchmarking {version}...");
            let samples = run_benchmark(binary, url, &dest_dir, options.runs)?;
            if samples.is_empty() {
                eprintln!("  Skipping {version} - all runs failed");
                continue;
            }
            let result = BenchmarkResult::new(version.clone(), samples);
            url_results.push(result);
        }

        url_results.sort_by(|a, b| a.mean.cmp(&b.mean));
        all_results.push(BenchmarkResultSet {
            url: url.clone(),
            url_name,
            results: url_results,
        });
    }

    if options.json {
        output_json_multi(&all_results)?;
    } else {
        output_table_multi(&all_results);
    }

    Ok(())
}
