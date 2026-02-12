//! Performance benchmarking command for comparing oc-rsync versions.
//!
//! This module provides the `benchmark` xtask command which compares performance
//! between upstream rsync, recent oc-rsync releases, and the current development
//! snapshot.
//!
//! Supports two modes:
//! - **Local**: Uses a local rsync daemon with Linux kernel source
//! - **Remote**: Tests against public rsync:// mirrors

use crate::cli::{BenchmarkArgs, BenchmarkMode};
use crate::error::{TaskError, TaskResult};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default benchmark data directory.
const DEFAULT_BENCH_DIR: &str = "/tmp/rsync-bench";

/// Default rsync daemon port.
const DEFAULT_DAEMON_PORT: u16 = 8873;

/// Number of benchmark runs per version.
const DEFAULT_RUNS: usize = 5;

/// Public rsync mirrors for remote benchmarking (not kernel.org).
///
/// Selected for diverse geography, file profiles, and practical transfer sizes.
const REMOTE_MIRRORS: &[RemoteMirror] = &[
    RemoteMirror {
        name: "GNU-hello",
        url: "rsync://ftp.gnu.org/gnu/hello/",
        description: "GNU hello — 44 files, ~12MB (US)",
    },
    RemoteMirror {
        name: "GNU-which",
        url: "rsync://ftp.gnu.org/gnu/which/",
        description: "GNU which — 15 files, ~1MB (US, connection overhead test)",
    },
    RemoteMirror {
        name: "Apache",
        url: "rsync://rsync.apache.org/apache-dist/httpd/",
        description: "Apache HTTPD dist — 55 files, ~35MB (US)",
    },
    RemoteMirror {
        name: "Berkeley",
        url: "rsync://mirrors.ocf.berkeley.edu/gnu/findutils/",
        description: "UC Berkeley GNU findutils — 47 files, ~34MB (US West)",
    },
    RemoteMirror {
        name: "CTAN",
        url: "rsync://rsync.dante.ctan.org/CTAN/macros/latex/base/",
        description: "CTAN LaTeX base — 314 files, ~54MB (Germany)",
    },
    RemoteMirror {
        name: "CPAN",
        url: "rsync://cpan-rsync.perl.org/CPAN/modules/by-module/HTTP/",
        description: "CPAN HTTP modules — varied sizes (US)",
    },
    RemoteMirror {
        name: "RIT-Arch",
        url: "rsync://mirrors.rit.edu/archlinux/core/os/x86_64/",
        description: "RIT Arch Linux core — ~400 files, ~300MB (US East)",
    },
    RemoteMirror {
        name: "Princeton",
        url: "rsync://mirror.math.princeton.edu/pub/slackware/slackware64-current/ChangeLog.txt",
        description: "Princeton Math single file — connection latency test (US East, Internet2)",
    },
    RemoteMirror {
        name: "OSUOSL",
        url: "rsync://rsync.osuosl.org/ubuntu/dists/noble/Release",
        description: "OSUOSL Ubuntu release file — connection test (US West)",
    },
];

/// A remote rsync mirror for benchmarking.
#[derive(Debug, Clone, Copy)]
struct RemoteMirror {
    name: &'static str,
    url: &'static str,
    description: &'static str,
}

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
    /// Benchmark mode: local or remote.
    pub mode: BenchmarkMode,
    /// Custom remote URLs for benchmarking.
    pub urls: Vec<String>,
    /// List available mirrors and exit.
    pub list_mirrors: bool,
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
            mode: BenchmarkMode::Local,
            urls: Vec::new(),
            list_mirrors: false,
        }
    }
}

impl From<BenchmarkArgs> for BenchmarkOptions {
    fn from(args: BenchmarkArgs) -> Self {
        Self {
            bench_dir: args
                .bench_dir
                .unwrap_or_else(|| PathBuf::from(DEFAULT_BENCH_DIR)),
            port: args.port.unwrap_or(DEFAULT_DAEMON_PORT),
            runs: args.runs.unwrap_or(DEFAULT_RUNS),
            versions: args.versions,
            skip_build: args.skip_build,
            json: args.json,
            mode: args.mode,
            urls: args.urls,
            list_mirrors: args.list_mirrors,
        }
    }
}

/// Result of a single benchmark iteration (timing + transfer stats).
#[derive(Clone, Debug)]
struct RunSample {
    elapsed: Duration,
    #[allow(dead_code)]
    bytes_sent: u64,
    bytes_received: u64,
    total_size: u64,
}

/// Result of a single benchmark run.
#[derive(Clone, Debug)]
struct BenchmarkResult {
    version: String,
    runs: Vec<Duration>,
    samples: Vec<RunSample>,
    mean: Duration,
    min: Duration,
    max: Duration,
    stddev: f64,
    mean_throughput_mbps: f64,
}

impl BenchmarkResult {
    fn new(version: String, samples: Vec<RunSample>) -> Self {
        if samples.is_empty() {
            return Self {
                version,
                runs: Vec::new(),
                samples: Vec::new(),
                mean: Duration::ZERO,
                min: Duration::ZERO,
                max: Duration::ZERO,
                stddev: 0.0,
                mean_throughput_mbps: 0.0,
            };
        }

        let runs: Vec<Duration> = samples.iter().map(|s| s.elapsed).collect();
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

        // Calculate mean throughput: total bytes received / total elapsed
        let total_bytes: u64 = samples.iter().map(|s| s.bytes_received).sum();
        let total_secs: f64 = samples.iter().map(|s| s.elapsed.as_secs_f64()).sum();
        let mean_throughput_mbps = if total_secs > 0.0 {
            (total_bytes as f64) / total_secs / (1024.0 * 1024.0)
        } else {
            0.0
        };

        Self {
            version,
            runs,
            samples,
            mean,
            min,
            max,
            stddev,
            mean_throughput_mbps,
        }
    }
}

/// Executes the benchmark command.
pub fn execute(workspace: &Path, options: BenchmarkOptions) -> TaskResult<()> {
    // Handle --list-mirrors
    if options.list_mirrors {
        return list_mirrors();
    }

    println!("=== oc-rsync Performance Benchmark ===\n");

    // Ensure benchmark directory exists
    fs::create_dir_all(&options.bench_dir).map_err(|e| {
        TaskError::Validation(format!(
            "failed to create benchmark directory {:?}: {}",
            options.bench_dir, e
        ))
    })?;

    // Determine benchmark URLs based on mode
    let urls = match options.mode {
        BenchmarkMode::Local => {
            // Check if daemon is running, start if needed
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
                // Use default public mirrors
                REMOTE_MIRRORS.iter().map(|m| m.url.to_string()).collect()
            } else {
                options.urls.clone()
            }
        }
    };

    println!("Mode: {:?}", options.mode);
    println!("URLs to benchmark:");
    for url in &urls {
        println!("  - {url}");
    }
    println!();

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

        // Sort by mean time
        url_results.sort_by(|a, b| a.mean.cmp(&b.mean));
        all_results.push(BenchmarkResultSet {
            url: url.clone(),
            url_name,
            results: url_results,
        });
    }

    // Output results
    if options.json {
        output_json_multi(&all_results)?;
    } else {
        output_table_multi(&all_results);
    }

    Ok(())
}

/// Lists available public rsync mirrors.
fn list_mirrors() -> TaskResult<()> {
    println!("=== Available Public Rsync Mirrors ===\n");
    println!("{:<12} {:<55} Description", "Name", "URL");
    println!("{}", "-".repeat(100));
    for mirror in REMOTE_MIRRORS {
        println!(
            "{:<12} {:<55} {}",
            mirror.name, mirror.url, mirror.description
        );
    }
    println!("\nUsage: cargo xtask benchmark --mode remote [--url <custom-url>]");
    Ok(())
}

/// Extracts a short name from a URL for display.
fn url_short_name(url: &str) -> String {
    // Check predefined mirrors first
    for mirror in REMOTE_MIRRORS {
        if url == mirror.url {
            return mirror.name.to_string();
        }
    }
    // Extract hostname
    url.strip_prefix("rsync://")
        .and_then(|s| s.split('/').next())
        .unwrap_or("unknown")
        .to_string()
}

/// Result set for a single URL.
#[derive(Debug)]
struct BenchmarkResultSet {
    url: String,
    url_name: String,
    results: Vec<BenchmarkResult>,
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
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_dir.to_str().unwrap(),
        ])
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
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_dir.to_str().unwrap(),
        ])
        .current_dir(workspace)
        .status();

    Ok(())
}

/// Runs a benchmark and returns timing + throughput results.
fn run_benchmark(
    binary: &Path,
    source_url: &str,
    dest_dir: &Path,
    runs: usize,
) -> TaskResult<Vec<RunSample>> {
    let mut results = Vec::with_capacity(runs);

    for i in 0..runs {
        // Clean destination
        let _ = fs::remove_dir_all(dest_dir);
        fs::create_dir_all(dest_dir)?;

        // Sync to drop caches (best effort)
        let _ = Command::new("sync").status();

        let start = Instant::now();
        let output = Command::new(binary)
            .args(["-a", "--stats", source_url, dest_dir.to_str().unwrap()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;

        let elapsed = start.elapsed();

        if !output.status.success() {
            eprintln!(
                "  Run {} failed with exit code {:?}",
                i + 1,
                output.status.code()
            );
            continue;
        }

        let stats_text = String::from_utf8_lossy(&output.stdout);
        let sample = parse_stats_output(&stats_text, elapsed);

        let throughput = if elapsed.as_secs_f64() > 0.0 {
            sample.bytes_received as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0)
        } else {
            0.0
        };

        println!(
            "  Run {}: {:.3}s  ({:.2} MB/s, {} bytes received)",
            i + 1,
            elapsed.as_secs_f64(),
            throughput,
            sample.bytes_received,
        );

        results.push(sample);
    }

    Ok(results)
}

/// Parses rsync `--stats` output to extract transfer metrics.
fn parse_stats_output(output: &str, elapsed: Duration) -> RunSample {
    let mut bytes_sent = 0u64;
    let mut bytes_received = 0u64;
    let mut total_size = 0u64;

    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Total bytes sent:") {
            bytes_sent = parse_stat_value(rest);
        } else if let Some(rest) = line.strip_prefix("Total bytes received:") {
            bytes_received = parse_stat_value(rest);
        } else if let Some(rest) = line.strip_prefix("Total file size:") {
            total_size = parse_stat_value(rest);
        }
    }

    RunSample {
        elapsed,
        bytes_sent,
        bytes_received,
        total_size,
    }
}

/// Parses a numeric value from stats output, stripping commas and unit suffixes.
fn parse_stat_value(s: &str) -> u64 {
    // Format is typically "  1,234,567 bytes" or "  1,234,567"
    s.split_whitespace()
        .next()
        .unwrap_or("0")
        .replace(',', "")
        .parse()
        .unwrap_or(0)
}

/// Outputs results for multiple URLs as formatted tables.
fn output_table_multi(result_sets: &[BenchmarkResultSet]) {
    println!("\n=== Benchmark Results ===\n");

    for result_set in result_sets {
        println!("--- {} ({}) ---\n", result_set.url_name, result_set.url);
        println!(
            "{:<12} {:>10} {:>10} {:>10} {:>10} {:>12}",
            "Version", "Mean", "Min", "Max", "Stddev", "Throughput"
        );
        println!("{}", "-".repeat(68));

        let baseline = result_set
            .results
            .first()
            .map(|r| r.mean)
            .unwrap_or(Duration::ZERO);

        for result in &result_set.results {
            let diff_pct = if baseline > Duration::ZERO {
                ((result.mean.as_secs_f64() - baseline.as_secs_f64()) / baseline.as_secs_f64())
                    * 100.0
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

            let throughput_str = if result.mean_throughput_mbps > 0.0 {
                format!("{:.2} MB/s", result.mean_throughput_mbps)
            } else {
                "-".to_string()
            };

            println!(
                "{:<12} {:>10.3}s {:>10.3}s {:>10.3}s {:>10.4}s {:>12}{}",
                result.version,
                result.mean.as_secs_f64(),
                result.min.as_secs_f64(),
                result.max.as_secs_f64(),
                result.stddev,
                throughput_str,
                speedup_str
            );
        }
        println!();
    }

    // Print summary comparison
    if result_sets.len() > 1 {
        print_summary_table(result_sets);
    }
}

/// Prints a summary table comparing all versions across all URLs.
fn print_summary_table(result_sets: &[BenchmarkResultSet]) {
    println!("=== Summary (Mean times in seconds) ===\n");

    // Collect all unique versions
    let mut versions: Vec<String> = Vec::new();
    for result_set in result_sets {
        for result in &result_set.results {
            if !versions.contains(&result.version) {
                versions.push(result.version.clone());
            }
        }
    }

    // Print header
    print!("{:<12}", "URL");
    for version in &versions {
        print!(" {version:>12}");
    }
    println!();
    println!("{}", "-".repeat(12 + 13 * versions.len()));

    // Print rows
    for result_set in result_sets {
        print!("{:<12}", result_set.url_name);
        for version in &versions {
            let time = result_set
                .results
                .iter()
                .find(|r| r.version == *version)
                .map(|r| format!("{:.3}s", r.mean.as_secs_f64()))
                .unwrap_or_else(|| "-".to_string());
            print!(" {time:>12}");
        }
        println!();
    }
    println!();
}

/// Outputs results for multiple URLs as JSON.
fn output_json_multi(result_sets: &[BenchmarkResultSet]) -> TaskResult<()> {
    println!("{{");
    println!("  \"benchmarks\": [");
    for (i, result_set) in result_sets.iter().enumerate() {
        let outer_comma = if i < result_sets.len() - 1 { "," } else { "" };
        println!("    {{");
        println!("      \"url\": \"{}\",", result_set.url);
        println!("      \"url_name\": \"{}\",", result_set.url_name);
        println!("      \"results\": [");
        for (j, result) in result_set.results.iter().enumerate() {
            let comma = if j < result_set.results.len() - 1 {
                ","
            } else {
                ""
            };
            let total_bytes: u64 = result.samples.iter().map(|s| s.bytes_received).sum();
            let total_size: u64 = result.samples.last().map(|s| s.total_size).unwrap_or(0);
            println!("        {{");
            println!("          \"version\": \"{}\",", result.version);
            println!(
                "          \"mean_ms\": {:.3},",
                result.mean.as_secs_f64() * 1000.0
            );
            println!(
                "          \"min_ms\": {:.3},",
                result.min.as_secs_f64() * 1000.0
            );
            println!(
                "          \"max_ms\": {:.3},",
                result.max.as_secs_f64() * 1000.0
            );
            println!("          \"stddev_ms\": {:.3},", result.stddev * 1000.0);
            println!(
                "          \"throughput_mbps\": {:.3},",
                result.mean_throughput_mbps
            );
            println!("          \"total_bytes_received\": {total_bytes},");
            println!("          \"total_file_size\": {total_size},");
            println!(
                "          \"runs_ms\": [{}]",
                result
                    .runs
                    .iter()
                    .map(|d| format!("{:.3}", d.as_secs_f64() * 1000.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("        }}{comma}");
        }
        println!("      ]");
        println!("    }}{outer_comma}");
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
        let samples = vec![
            RunSample {
                elapsed: Duration::from_millis(100),
                bytes_sent: 100,
                bytes_received: 1000,
                total_size: 2000,
            },
            RunSample {
                elapsed: Duration::from_millis(110),
                bytes_sent: 100,
                bytes_received: 1000,
                total_size: 2000,
            },
            RunSample {
                elapsed: Duration::from_millis(90),
                bytes_sent: 100,
                bytes_received: 1000,
                total_size: 2000,
            },
            RunSample {
                elapsed: Duration::from_millis(105),
                bytes_sent: 100,
                bytes_received: 1000,
                total_size: 2000,
            },
            RunSample {
                elapsed: Duration::from_millis(95),
                bytes_sent: 100,
                bytes_received: 1000,
                total_size: 2000,
            },
        ];
        let result = BenchmarkResult::new("test".to_string(), samples);

        assert_eq!(result.min, Duration::from_millis(90));
        assert_eq!(result.max, Duration::from_millis(110));
        // Mean should be 100ms
        assert!((result.mean.as_millis() as i64 - 100).abs() <= 1);
        assert!(result.mean_throughput_mbps > 0.0);
    }

    #[test]
    fn parse_stats_output_extracts_values() {
        let stats = "\
Number of files: 42 (reg: 40, dir: 2)
Total file size: 1,234,567 bytes
Total bytes sent: 456
Total bytes received: 1,234,567
";
        let sample = parse_stats_output(stats, Duration::from_secs(1));
        assert_eq!(sample.bytes_sent, 456);
        assert_eq!(sample.bytes_received, 1_234_567);
        assert_eq!(sample.total_size, 1_234_567);
    }

    #[test]
    fn parse_stat_value_handles_commas() {
        assert_eq!(parse_stat_value(" 1,234,567 bytes"), 1_234_567);
        assert_eq!(parse_stat_value(" 42"), 42);
        assert_eq!(parse_stat_value(" 0"), 0);
    }

    #[test]
    fn default_options_are_sensible() {
        let options = BenchmarkOptions::default();
        assert_eq!(options.port, DEFAULT_DAEMON_PORT);
        assert_eq!(options.runs, DEFAULT_RUNS);
        assert!(!options.skip_build);
    }
}
