//! Performance benchmarking command for comparing oc-rsync versions.
//!
//! This module provides the `benchmark` xtask command which compares performance
//! between upstream rsync, recent oc-rsync releases, and the current development
//! snapshot.

use crate::cli::BenchmarkArgs;
use crate::error::{TaskError, TaskResult};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default benchmark data directory.
const DEFAULT_BENCH_DIR: &str = "/tmp/rsync-bench";

/// Default rsync daemon port.
const DEFAULT_DAEMON_PORT: u16 = 8873;

/// Number of benchmark runs per version.
const DEFAULT_RUNS: usize = 5;

/// Benchmark configuration options.
#[derive(Clone, Debug)]
pub struct BenchmarkOptions {
    /// Directory for benchmark data and daemon.
    pub bench_dir: PathBuf,
    /// Rsync daemon port.
    pub port: u16,
    /// Number of runs per version.
    pub runs: usize,
    /// Versions to benchmark (empty = auto-detect).
    pub versions: Vec<String>,
    /// Skip building versions (use existing binaries).
    pub skip_build: bool,
    /// Output format.
    pub json: bool,
}

impl Default for BenchmarkOptions {
    fn default() -> Self {
        Self {
            bench_dir: PathBuf::from(DEFAULT_BENCH_DIR),
            port: DEFAULT_DAEMON_PORT,
            runs: DEFAULT_RUNS,
            versions: Vec::new(),
            skip_build: false,
            json: false,
        }
    }
}

impl From<BenchmarkArgs> for BenchmarkOptions {
    fn from(args: BenchmarkArgs) -> Self {
        Self {
            bench_dir: args.bench_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_BENCH_DIR)),
            port: args.port.unwrap_or(DEFAULT_DAEMON_PORT),
            runs: args.runs.unwrap_or(DEFAULT_RUNS),
            versions: args.versions,
            skip_build: args.skip_build,
            json: args.json,
        }
    }
}

/// Result of a single benchmark run.
#[derive(Clone, Debug)]
struct BenchmarkResult {
    version: String,
    runs: Vec<Duration>,
    mean: Duration,
    min: Duration,
    max: Duration,
    stddev: f64,
}

impl BenchmarkResult {
    fn new(version: String, runs: Vec<Duration>) -> Self {
        if runs.is_empty() {
            return Self {
                version,
                runs,
                mean: Duration::ZERO,
                min: Duration::ZERO,
                max: Duration::ZERO,
                stddev: 0.0,
            };
        }

        let mean = runs.iter().sum::<Duration>() / runs.len() as u32;
        let min = *runs.iter().min().unwrap_or(&Duration::ZERO);
        let max = *runs.iter().max().unwrap_or(&Duration::ZERO);

        let mean_secs = mean.as_secs_f64();
        let variance = runs
            .iter()
            .map(|d| {
                let diff = d.as_secs_f64() - mean_secs;
                diff * diff
            })
            .sum::<f64>()
            / runs.len() as f64;
        let stddev = variance.sqrt();

        Self {
            version,
            runs,
            mean,
            min,
            max,
            stddev,
        }
    }
}

/// Executes the benchmark command.
pub fn execute(workspace: &Path, options: BenchmarkOptions) -> TaskResult<()> {
    println!("=== oc-rsync Performance Benchmark ===\n");

    // Ensure benchmark directory exists
    fs::create_dir_all(&options.bench_dir).map_err(|e| {
        TaskError::Validation(format!(
            "failed to create benchmark directory {:?}: {}",
            options.bench_dir, e
        ))
    })?;

    // Check if daemon is running, start if needed
    let daemon_url = format!("rsync://localhost:{}/kernel/arch/x86/", options.port);
    if !check_daemon_running(options.port) {
        println!("Starting rsync daemon on port {}...", options.port);
        start_daemon(workspace, &options)?;
    } else {
        println!("Using existing rsync daemon on port {}", options.port);
    }

    // Determine versions to benchmark
    let versions = if options.versions.is_empty() {
        detect_versions(workspace)?
    } else {
        options.versions.clone()
    };

    println!("Versions to benchmark: {versions:?}\n");

    // Build versions if needed
    let mut binaries: HashMap<String, PathBuf> = HashMap::new();

    // Always include upstream rsync
    if let Some(rsync_path) = find_upstream_rsync() {
        binaries.insert("upstream".to_string(), rsync_path);
    }

    // Build current development version
    if !options.skip_build {
        println!("Building current development version...");
        build_release(workspace)?;
    }
    let current_binary = workspace.join("target/release/oc-rsync");
    if current_binary.exists() {
        binaries.insert("dev".to_string(), current_binary);
    }

    // Build tagged versions
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

    // Run benchmarks
    let dest_dir = options.bench_dir.join("bench-dest");
    let mut results: Vec<BenchmarkResult> = Vec::new();

    for (version, binary) in &binaries {
        println!("\nBenchmarking {version}...");
        let runs = run_benchmark(binary, &daemon_url, &dest_dir, options.runs)?;
        if runs.is_empty() {
            eprintln!("  Skipping {version} - all runs failed");
            continue;
        }
        let result = BenchmarkResult::new(version.clone(), runs);
        results.push(result);
    }

    // Sort by mean time
    results.sort_by(|a, b| a.mean.cmp(&b.mean));

    // Output results
    if options.json {
        output_json(&results)?;
    } else {
        output_table(&results);
    }

    Ok(())
}

/// Checks if rsync daemon is running on the given port.
fn check_daemon_running(port: u16) -> bool {
    use std::net::TcpStream;
    // Try IPv4 first, then IPv6
    TcpStream::connect(format!("127.0.0.1:{port}"))
        .or_else(|_| TcpStream::connect(format!("[::1]:{port}")))
        .map(|_| true)
        .unwrap_or(false)
}

/// Stops any existing rsync daemon.
fn stop_daemon(options: &BenchmarkOptions) {
    let pid_path = options.bench_dir.join("rsyncd.pid");
    if let Ok(pid_str) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // Kill existing daemon
            let _ = Command::new("kill").arg(pid.to_string()).status();
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // Remove stale pid file
    let _ = fs::remove_file(&pid_path);
}

/// Starts an rsync daemon for benchmarking.
fn start_daemon(workspace: &Path, options: &BenchmarkOptions) -> TaskResult<()> {
    // Stop any existing daemon first
    stop_daemon(options);

    let conf_path = options.bench_dir.join("rsyncd.conf");
    let pid_path = options.bench_dir.join("rsyncd.pid");
    let log_path = options.bench_dir.join("rsyncd.log");

    // Check if kernel source exists, download if not
    let kernel_dir = options.bench_dir.join("kernel-src");
    if !kernel_dir.exists() {
        println!("Downloading Linux kernel source for benchmarking...");
        download_kernel_source(&options.bench_dir)?;
    }

    // Write daemon config
    let config = format!(
        r#"port = {}
pid file = {}
log file = {}
use chroot = no
read only = yes

[kernel]
    path = {}
    comment = Linux kernel source for benchmarking
"#,
        options.port,
        pid_path.display(),
        log_path.display(),
        kernel_dir.display()
    );

    fs::write(&conf_path, config)?;

    // Start daemon
    let status = Command::new("rsync")
        .args(["--daemon", "--config", conf_path.to_str().unwrap()])
        .current_dir(workspace)
        .status()?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: "rsync".into(),
            status,
        });
    }

    // Wait for daemon to be ready
    std::thread::sleep(Duration::from_millis(500));

    Ok(())
}

/// Downloads Linux kernel source for benchmarking.
fn download_kernel_source(bench_dir: &Path) -> TaskResult<()> {
    let tarball = bench_dir.join("linux-6.12.tar.xz");
    let kernel_dir = bench_dir.join("kernel-src");

    // Download if not exists
    if !tarball.exists() {
        let status = Command::new("curl")
            .args([
                "-L",
                "-o",
                tarball.to_str().unwrap(),
                "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.tar.xz",
            ])
            .status()?;

        if !status.success() {
            return Err(TaskError::CommandFailed {
                program: "curl".into(),
                status,
            });
        }
    }

    // Extract
    fs::create_dir_all(&kernel_dir).ok();
    let status = Command::new("tar")
        .args([
            "-xf",
            tarball.to_str().unwrap(),
            "-C",
            kernel_dir.to_str().unwrap(),
            "--strip-components=1",
        ])
        .status()?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: "tar".into(),
            status,
        });
    }

    Ok(())
}

/// Detects recent release versions from git tags.
/// Only considers v0.x.x tags (oc-rsync releases), not v3.x.x (rsync compatibility tags).
fn detect_versions(workspace: &Path) -> TaskResult<Vec<String>> {
    let output = Command::new("git")
        .args(["tag", "-l", "v0.*", "--sort=-v:refname"])
        .current_dir(workspace)
        .output()?;

    let tags: Vec<String> = BufReader::new(&output.stdout[..])
        .lines()
        .map_while(|l| l.ok())
        .take(3) // Last 3 releases
        .collect();

    Ok(tags)
}

/// Finds the upstream rsync binary.
fn find_upstream_rsync() -> Option<PathBuf> {
    Command::new("which")
        .arg("rsync")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| PathBuf::from(s.trim()))
            } else {
                None
            }
        })
}

/// Builds the current release version.
fn build_release(workspace: &Path) -> TaskResult<()> {
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(workspace)
        .status()?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: "cargo build --release".into(),
            status,
        });
    }

    Ok(())
}

/// Builds a specific tagged version.
fn build_version(workspace: &Path, version: &str, output: &Path) -> TaskResult<()> {
    // Create a worktree for building the version
    let worktree_dir = workspace.join("target").join(format!("worktree-{version}"));

    // Clean up existing worktree
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", worktree_dir.to_str().unwrap()])
        .current_dir(workspace)
        .status();

    // Create worktree
    let status = Command::new("git")
        .args(["worktree", "add", worktree_dir.to_str().unwrap(), version])
        .current_dir(workspace)
        .status()?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: format!("git worktree add {version}"),
            status,
        });
    }

    // Build in worktree
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&worktree_dir)
        .status()?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: format!("cargo build {version} --release"),
            status,
        });
    }

    // Copy binary
    let binary = worktree_dir.join("target/release/oc-rsync");
    fs::copy(&binary, output)?;

    // Clean up worktree
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", worktree_dir.to_str().unwrap()])
        .current_dir(workspace)
        .status();

    Ok(())
}

/// Runs a benchmark and returns timing results.
fn run_benchmark(
    binary: &Path,
    source_url: &str,
    dest_dir: &Path,
    runs: usize,
) -> TaskResult<Vec<Duration>> {
    let mut results = Vec::with_capacity(runs);

    for i in 0..runs {
        // Clean destination
        let _ = fs::remove_dir_all(dest_dir);
        fs::create_dir_all(dest_dir)?;

        // Sync to drop caches (best effort)
        let _ = Command::new("sync").status();

        let start = Instant::now();
        let status = Command::new(binary)
            .args(["-a", source_url, dest_dir.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;

        let elapsed = start.elapsed();

        if !status.success() {
            eprintln!("  Run {} failed with exit code {:?}", i + 1, status.code());
            continue;
        }

        print!("  Run {}: {:.3}s", i + 1, elapsed.as_secs_f64());
        std::io::stdout().flush().ok();
        println!();

        results.push(elapsed);
    }

    Ok(results)
}

/// Outputs results as a formatted table.
fn output_table(results: &[BenchmarkResult]) {
    println!("\n=== Benchmark Results ===\n");
    println!(
        "{:<12} {:>10} {:>10} {:>10} {:>10}",
        "Version", "Mean", "Min", "Max", "Stddev"
    );
    println!("{}", "-".repeat(54));

    let baseline = results.first().map(|r| r.mean).unwrap_or(Duration::ZERO);

    for result in results {
        let diff_pct = if baseline > Duration::ZERO {
            ((result.mean.as_secs_f64() - baseline.as_secs_f64()) / baseline.as_secs_f64()) * 100.0
        } else {
            0.0
        };

        let speedup_str = if diff_pct.abs() < 1.0 {
            String::new()
        } else if diff_pct > 0.0 {
            format!(" ({diff_pct:.1}% slower)")
        } else {
            let faster = -diff_pct;
            format!(" ({faster:.1}% faster)")
        };

        println!(
            "{:<12} {:>10.3}s {:>10.3}s {:>10.3}s {:>10.4}s{}",
            result.version,
            result.mean.as_secs_f64(),
            result.min.as_secs_f64(),
            result.max.as_secs_f64(),
            result.stddev,
            speedup_str
        );
    }
}

/// Outputs results as JSON.
fn output_json(results: &[BenchmarkResult]) -> TaskResult<()> {
    println!("{{");
    println!("  \"results\": [");
    for (i, result) in results.iter().enumerate() {
        let comma = if i < results.len() - 1 { "," } else { "" };
        println!("    {{");
        println!("      \"version\": \"{}\",", result.version);
        println!("      \"mean_ms\": {:.3},", result.mean.as_secs_f64() * 1000.0);
        println!("      \"min_ms\": {:.3},", result.min.as_secs_f64() * 1000.0);
        println!("      \"max_ms\": {:.3},", result.max.as_secs_f64() * 1000.0);
        println!("      \"stddev_ms\": {:.3},", result.stddev * 1000.0);
        println!(
            "      \"runs_ms\": [{}]",
            result
                .runs
                .iter()
                .map(|d| format!("{:.3}", d.as_secs_f64() * 1000.0))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("    }}{comma}");
    }
    println!("  ]");
    println!("}}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_result_calculates_stats() {
        let runs = vec![
            Duration::from_millis(100),
            Duration::from_millis(110),
            Duration::from_millis(90),
            Duration::from_millis(105),
            Duration::from_millis(95),
        ];
        let result = BenchmarkResult::new("test".to_string(), runs);

        assert_eq!(result.min, Duration::from_millis(90));
        assert_eq!(result.max, Duration::from_millis(110));
        // Mean should be 100ms
        assert!((result.mean.as_millis() as i64 - 100).abs() <= 1);
    }

    #[test]
    fn default_options_are_sensible() {
        let options = BenchmarkOptions::default();
        assert_eq!(options.port, DEFAULT_DAEMON_PORT);
        assert_eq!(options.runs, DEFAULT_RUNS);
        assert!(!options.skip_build);
    }
}
