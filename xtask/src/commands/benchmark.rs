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

use crate::cli::{BenchmarkArgs, BenchmarkMode, DataProfile};
use crate::error::{TaskError, TaskResult};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Default benchmark data directory.
const DEFAULT_BENCH_DIR: &str = "/tmp/rsync-bench";

/// Default rsync daemon port.
const DEFAULT_DAEMON_PORT: u16 = 8873;

/// Number of benchmark runs per version.
const DEFAULT_RUNS: usize = 5;

/// Default base port for loopback mode daemons.
const DEFAULT_LOOPBACK_PORT: u16 = 18873;

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
    /// Data profile for loopback mode test data generation.
    pub data_profile: DataProfile,
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
            data_profile: DataProfile::Medium,
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
            data_profile: args.data_profile,
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

    // Loopback mode has its own orchestration flow
    if options.mode == BenchmarkMode::Loopback {
        return run_loopback_benchmarks(workspace, &options);
    }

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
        BenchmarkMode::Loopback => unreachable!("handled above"),
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

// ─────────────────────────────────────────────────────────────────────────────
// Loopback benchmark mode
// ─────────────────────────────────────────────────────────────────────────────

/// RAII guard that kills a daemon process and removes its pid file on drop.
struct DaemonGuard {
    #[allow(dead_code)]
    name: String,
    child: Option<Child>,
    pid_file: PathBuf,
    conf_file: PathBuf,
}

impl DaemonGuard {
    fn new(name: String, child: Child, pid_file: PathBuf, conf_file: PathBuf) -> Self {
        Self {
            name,
            child: Some(child),
            pid_file,
            conf_file,
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(&self.pid_file);
        let _ = fs::remove_file(&self.conf_file);
    }
}

/// Returns the file count and directory count for a given data profile.
fn profile_params(profile: DataProfile) -> (usize, usize) {
    match profile {
        DataProfile::Small => (1_000, 10),
        DataProfile::Medium => (10_000, 100),
        DataProfile::Large => (50_000, 500),
    }
}

/// Generates deterministic test data for loopback benchmarks.
///
/// Creates a mix of file sizes in nested directories using a seeded PRNG
/// for reproducibility. Skips generation if a marker file indicates the
/// requested profile already exists.
fn generate_loopback_data(bench_dir: &Path, profile: DataProfile) -> TaskResult<PathBuf> {
    let data_dir = bench_dir.join("loopback-data");
    let marker = data_dir.join(".profile");

    // Check if data already matches requested profile
    let profile_name = format!("{profile:?}");
    if marker.exists() {
        if let Ok(existing) = fs::read_to_string(&marker) {
            if existing.trim() == profile_name {
                println!("Reusing existing {profile_name} test data in {}", data_dir.display());
                return Ok(data_dir);
            }
        }
    }

    // Clean and regenerate
    let _ = fs::remove_dir_all(&data_dir);
    fs::create_dir_all(&data_dir)?;

    let (file_count, dir_count) = profile_params(profile);
    println!(
        "Generating {profile_name} test data: {file_count} files in {dir_count} dirs..."
    );

    // Create directory structure (2 levels)
    let dirs_per_level = (dir_count as f64).sqrt().ceil() as usize;
    let mut dir_paths: Vec<PathBuf> = Vec::with_capacity(dir_count);
    for i in 0..dirs_per_level {
        let level1 = data_dir.join(format!("d{i:04}"));
        fs::create_dir_all(&level1)?;
        dir_paths.push(level1.clone());
        for j in 0..dirs_per_level {
            if dir_paths.len() >= dir_count {
                break;
            }
            let level2 = level1.join(format!("s{j:04}"));
            fs::create_dir_all(&level2)?;
            dir_paths.push(level2);
        }
        if dir_paths.len() >= dir_count {
            break;
        }
    }

    // Simple seeded PRNG (xorshift64)
    let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_BABE;
    let mut next_rand = move || -> u64 {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        rng_state
    };

    // Generate files with varied sizes:
    // 5% empty, 30% tiny (<1KB), 40% small (1-10KB), 20% medium (10-100KB), 5% large (100KB-1MB)
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB reusable buffer
    for i in 0..file_count {
        let dir = &dir_paths[i % dir_paths.len()];
        let file_path = dir.join(format!("f{i:06}.dat"));

        let r = (next_rand() % 100) as u32;
        let size = match r {
            0..5 => 0,                                           // empty
            5..35 => (next_rand() % 1024) as usize,             // tiny: 0-1023
            35..75 => 1024 + (next_rand() % (9 * 1024)) as usize,  // small: 1KB-10KB
            75..95 => 10 * 1024 + (next_rand() % (90 * 1024)) as usize, // medium: 10-100KB
            _ => 100 * 1024 + (next_rand() % (900 * 1024)) as usize,    // large: 100KB-1MB
        };

        if size == 0 {
            fs::File::create(&file_path)?;
        } else {
            // Fill buffer with deterministic data
            for chunk in buf[..size].chunks_mut(8) {
                let val = next_rand();
                let bytes = val.to_le_bytes();
                let copy_len = chunk.len().min(8);
                chunk[..copy_len].copy_from_slice(&bytes[..copy_len]);
            }
            let mut f = fs::File::create(&file_path)?;
            f.write_all(&buf[..size])?;
        }

        if (i + 1) % 5000 == 0 {
            println!("  Generated {}/{file_count} files...", i + 1);
        }
    }

    // Write marker
    fs::write(&marker, &profile_name)?;

    let total_bytes: u64 = walkdir_size(&data_dir);
    println!(
        "Generated {file_count} files ({:.1} MB) in {}",
        total_bytes as f64 / (1024.0 * 1024.0),
        data_dir.display()
    );

    Ok(data_dir)
}

/// Calculates the total size of all files in a directory tree.
fn walkdir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += walkdir_size(&path);
            } else if let Ok(meta) = path.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// Waits for a TCP port to become reachable on localhost.
fn wait_for_port(port: u16, timeout: Duration) -> bool {
    use std::net::TcpStream;
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Checks that a port is available for binding.
fn port_available(port: u16) -> bool {
    use std::net::TcpListener;
    TcpListener::bind(format!("127.0.0.1:{port}")).is_ok()
}

/// Starts a daemon from the given binary on the specified port.
///
/// Returns a `DaemonGuard` that kills the daemon on drop.
fn start_loopback_daemon(
    name: &str,
    binary: &Path,
    data_dir: &Path,
    port: u16,
    bench_dir: &Path,
    is_oc_rsync: bool,
) -> TaskResult<DaemonGuard> {
    let conf_path = bench_dir.join(format!("loopback-{name}.conf"));
    let pid_path = bench_dir.join(format!("loopback-{name}.pid"));
    let log_path = bench_dir.join(format!("loopback-{name}.log"));

    // Write daemon config (oc-rsync requires [daemon] section, upstream uses bare directives)
    let config = if is_oc_rsync {
        format!(
            r#"[daemon]
path = {data}
pid file = {pid}
log file = {log}
port = {port}
use chroot = false

[bench]
path = {data}
comment = Loopback benchmark data
read only = true
"#,
            pid = pid_path.display(),
            log = log_path.display(),
            data = data_dir.display(),
        )
    } else {
        format!(
            r#"port = {port}
pid file = {pid}
log file = {log}
use chroot = no
read only = yes

[bench]
    path = {data}
    comment = Loopback benchmark data
"#,
            pid = pid_path.display(),
            log = log_path.display(),
            data = data_dir.display(),
        )
    };
    fs::write(&conf_path, &config)?;

    // Start daemon
    let mut cmd = Command::new(binary);
    cmd.args(["--daemon", "--no-detach", "--config"])
        .arg(&conf_path)
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    // Force native daemon mode for oc-rsync (no fallback to system rsync)
    if is_oc_rsync {
        cmd.env("OC_RSYNC_DAEMON_FALLBACK", "0");
    }

    let child = cmd.spawn().map_err(|e| {
        TaskError::Validation(format!("failed to start {name} daemon on port {port}: {e}"))
    })?;

    let guard = DaemonGuard::new(name.to_string(), child, pid_path, conf_path);

    // Wait for daemon to be ready
    if !wait_for_port(port, Duration::from_secs(5)) {
        return Err(TaskError::Validation(format!(
            "{name} daemon failed to start on port {port} within 5 seconds"
        )));
    }

    println!("  Started {name} daemon on port {port}");
    Ok(guard)
}

/// Tries to start a daemon, returning `Ok(None)` on failure instead of an error.
fn try_start_loopback_daemon(
    name: &str,
    binary: &Path,
    data_dir: &Path,
    port: u16,
    bench_dir: &Path,
    is_oc_rsync: bool,
) -> TaskResult<Option<DaemonGuard>> {
    match start_loopback_daemon(name, binary, data_dir, port, bench_dir, is_oc_rsync) {
        Ok(guard) => Ok(Some(guard)),
        Err(e) => {
            eprintln!("  Warning: {name} daemon failed to start: {e}");
            Ok(None)
        }
    }
}

/// Orchestrates the full loopback benchmark flow.
///
/// Uses oc-rsync dev as the reference daemon for client benchmarks, since the
/// system rsync may be openrsync (macOS) which has daemon compatibility issues.
/// Each oc-rsync version also runs as a daemon for server benchmarks, tested
/// with the dev client.
fn run_loopback_benchmarks(workspace: &Path, options: &BenchmarkOptions) -> TaskResult<()> {
    let base_port = options.port.max(DEFAULT_LOOPBACK_PORT);

    println!("=== Loopback Benchmark Mode ===\n");
    println!("Profile: {:?}", options.data_profile);
    println!("Runs: {}", options.runs);
    println!("Base port: {base_port}\n");

    // Step 1: Generate test data
    let data_dir = generate_loopback_data(&options.bench_dir, options.data_profile)?;

    // Step 2: Determine versions and build binaries
    let versions = if options.versions.is_empty() {
        detect_versions(workspace)?
    } else {
        options.versions.clone()
    };
    println!("\nVersions to benchmark: {versions:?}");

    let mut binaries: HashMap<String, PathBuf> = HashMap::new();

    // Upstream rsync (for client benchmarks only — may be openrsync)
    if let Some(rsync_path) = find_upstream_rsync() {
        binaries.insert("upstream".to_string(), rsync_path);
    }

    // Current dev build
    if !options.skip_build {
        println!("\nBuilding current development version...");
        build_release(workspace)?;
    }
    let current_binary = workspace.join("target/release/oc-rsync");
    if current_binary.exists() {
        binaries.insert("dev".to_string(), current_binary);
    }

    // Tagged versions
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

    if binaries.is_empty() {
        return Err(TaskError::Validation(
            "no binaries available for benchmarking".into(),
        ));
    }

    // We need a dev binary for the reference daemon
    let dev_binary = binaries.get("dev").cloned();
    if dev_binary.is_none() {
        return Err(TaskError::Validation(
            "dev binary required for loopback mode (reference daemon)".into(),
        ));
    }
    let dev_binary = dev_binary.unwrap();

    // Collect oc-rsync version names (non-upstream) for per-version daemons
    let mut oc_versions: Vec<String> = binaries
        .keys()
        .filter(|k| *k != "upstream")
        .cloned()
        .collect();
    oc_versions.sort();

    // Step 3: Verify ports are available and allocate
    // Port layout: base_port = reference daemon (dev), then one per oc-rsync version
    let reference_port = base_port;
    if !port_available(reference_port) {
        return Err(TaskError::Validation(format!(
            "port {reference_port} is already in use"
        )));
    }

    let mut version_ports: HashMap<String, u16> = HashMap::new();
    for (idx, version) in oc_versions.iter().enumerate() {
        let port = base_port + 2 * (idx as u16 + 1);
        if !port_available(port) {
            return Err(TaskError::Validation(format!(
                "port {port} (for {version} daemon) is already in use"
            )));
        }
        version_ports.insert(version.clone(), port);
    }

    // Step 4: Start daemons
    println!("\nStarting daemons...");
    let mut guards: Vec<DaemonGuard> = Vec::new();

    // Start reference daemon (dev) for client benchmarks
    let ref_guard = start_loopback_daemon(
        "ref-dev",
        &dev_binary,
        &data_dir,
        reference_port,
        &options.bench_dir,
        true,
    )?;
    guards.push(ref_guard);

    // Start per-version daemons for server benchmarks
    let mut daemon_started: HashMap<String, bool> = HashMap::new();
    for version in &oc_versions {
        let port = version_ports[version];
        let binary = &binaries[version];
        match try_start_loopback_daemon(
            version,
            binary,
            &data_dir,
            port,
            &options.bench_dir,
            true,
        )? {
            Some(guard) => {
                guards.push(guard);
                daemon_started.insert(version.clone(), true);
            }
            None => {
                daemon_started.insert(version.clone(), false);
            }
        }
    }

    let dest_dir = options.bench_dir.join("loopback-dest");
    let client_url = format!("rsync://127.0.0.1:{reference_port}/bench/");

    // Step 5: Warmup
    println!("\nRunning warmup...");
    for (version, binary) in &binaries {
        let _ = fs::remove_dir_all(&dest_dir);
        fs::create_dir_all(&dest_dir)?;
        let output = Command::new(binary)
            .args(["-a", &client_url, dest_dir.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match output {
            Ok(o) if o.status.success() => println!("  Warmup OK: {version}"),
            _ => println!("  Warmup skipped: {version} (client connection failed)"),
        }
    }

    // Step 6: Client benchmarks (each version's client → dev reference daemon)
    println!("\n=== Client Benchmarks (version as client → dev reference daemon) ===");
    let mut client_results: Vec<BenchmarkResult> = Vec::new();

    for (version, binary) in &binaries {
        println!("\nBenchmarking {version} (client)...");
        let samples = run_benchmark(binary, &client_url, &dest_dir, options.runs)?;
        if samples.is_empty() {
            eprintln!("  Skipping {version} - all runs failed");
            continue;
        }
        client_results.push(BenchmarkResult::new(version.clone(), samples));
    }
    client_results.sort_by(|a, b| a.mean.cmp(&b.mean));

    // Step 7: Server benchmarks (dev client → each version's daemon)
    println!("\n=== Server Benchmarks (dev client → version as daemon) ===");
    let mut server_results: Vec<BenchmarkResult> = Vec::new();

    for version in &oc_versions {
        if !daemon_started.get(version).copied().unwrap_or(false) {
            continue;
        }
        let port = version_ports[version];
        let server_url = format!("rsync://127.0.0.1:{port}/bench/");
        println!("\nBenchmarking {version} daemon (server)...");
        let samples = run_benchmark(&dev_binary, &server_url, &dest_dir, options.runs)?;
        if samples.is_empty() {
            eprintln!("  Skipping {version} daemon - all runs failed");
            continue;
        }
        server_results.push(BenchmarkResult::new(version.clone(), samples));
    }
    server_results.sort_by(|a, b| a.mean.cmp(&b.mean));

    // Step 8: Output results
    if options.json {
        output_loopback_json(&client_results, &server_results)?;
    } else {
        output_loopback_table(&client_results, &server_results);
    }

    // Guards are dropped here, killing all daemons
    println!("\nStopping daemons...");
    drop(guards);
    println!("Done.");

    Ok(())
}

/// Outputs loopback results as formatted tables.
fn output_loopback_table(
    client_results: &[BenchmarkResult],
    server_results: &[BenchmarkResult],
) {
    println!("\n=== Loopback Benchmark Results ===");

    println!("\n--- Client Performance (version as client → dev reference daemon) ---\n");
    print_result_table(client_results);

    if !server_results.is_empty() {
        println!("--- Server Performance (dev client → version as daemon) ---\n");
        print_result_table(server_results);
    }
}

/// Prints a formatted table for a set of benchmark results.
fn print_result_table(results: &[BenchmarkResult]) {
    println!(
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>12}",
        "Version", "Mean", "Min", "Max", "Stddev", "Throughput"
    );
    println!("{}", "-".repeat(68));

    let baseline = results.first().map(|r| r.mean).unwrap_or(Duration::ZERO);

    for result in results {
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

/// Outputs loopback results as JSON.
fn output_loopback_json(
    client_results: &[BenchmarkResult],
    server_results: &[BenchmarkResult],
) -> TaskResult<()> {
    println!("{{");
    println!("  \"mode\": \"loopback\",");

    println!("  \"client_benchmarks\": [");
    print_json_results(client_results);
    println!("  ],");

    println!("  \"server_benchmarks\": [");
    print_json_results(server_results);
    println!("  ]");

    println!("}}");
    Ok(())
}

/// Prints benchmark results as JSON array elements.
fn print_json_results(results: &[BenchmarkResult]) {
    for (j, result) in results.iter().enumerate() {
        let comma = if j < results.len() - 1 { "," } else { "" };
        let total_bytes: u64 = result.samples.iter().map(|s| s.bytes_received).sum();
        let total_size: u64 = result.samples.last().map(|s| s.total_size).unwrap_or(0);
        println!("    {{");
        println!("      \"version\": \"{}\",", result.version);
        println!(
            "      \"mean_ms\": {:.3},",
            result.mean.as_secs_f64() * 1000.0
        );
        println!(
            "      \"min_ms\": {:.3},",
            result.min.as_secs_f64() * 1000.0
        );
        println!(
            "      \"max_ms\": {:.3},",
            result.max.as_secs_f64() * 1000.0
        );
        println!("      \"stddev_ms\": {:.3},", result.stddev * 1000.0);
        println!(
            "      \"throughput_mbps\": {:.3},",
            result.mean_throughput_mbps
        );
        println!("      \"total_bytes_received\": {total_bytes},");
        println!("      \"total_file_size\": {total_size},");
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
        assert_eq!(options.data_profile, DataProfile::Medium);
    }

    #[test]
    fn profile_params_returns_expected_sizes() {
        assert_eq!(profile_params(DataProfile::Small), (1_000, 10));
        assert_eq!(profile_params(DataProfile::Medium), (10_000, 100));
        assert_eq!(profile_params(DataProfile::Large), (50_000, 500));
    }

    #[test]
    fn generate_loopback_data_creates_files() {
        let tmp = std::env::temp_dir().join("xtask-bench-test-gen");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Use small profile but verify structure
        let data_dir = generate_loopback_data(&tmp, DataProfile::Small).unwrap();
        assert!(data_dir.exists());
        assert!(data_dir.join(".profile").exists());

        let marker = fs::read_to_string(data_dir.join(".profile")).unwrap();
        assert_eq!(marker.trim(), "Small");

        // Verify some files were created
        let total = walkdir_size(&data_dir);
        assert!(total > 0, "expected non-zero total size");

        // Verify idempotency (second call reuses existing data)
        let data_dir2 = generate_loopback_data(&tmp, DataProfile::Small).unwrap();
        assert_eq!(data_dir, data_dir2);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn walkdir_size_counts_files() {
        let tmp = std::env::temp_dir().join("xtask-bench-test-walkdir");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub")).unwrap();
        fs::write(tmp.join("a.txt"), "hello").unwrap();
        fs::write(tmp.join("sub/b.txt"), "world!").unwrap();

        let total = walkdir_size(&tmp);
        assert_eq!(total, 11); // "hello" (5) + "world!" (6)

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn port_available_finds_open_port() {
        // Port 0 is never bindable in the normal sense, but high ports should be available
        // Just verify the function doesn't panic
        let _ = port_available(59999);
    }
}
