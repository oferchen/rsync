//! Benchmark execution: timing, daemon lifecycle, version builds, and stat parsing.

use crate::error::{TaskError, TaskResult};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::config::BenchmarkOptions;

/// Result of a single benchmark iteration (timing + transfer stats).
#[derive(Clone, Debug)]
pub(super) struct RunSample {
    pub(super) elapsed: Duration,
    #[allow(dead_code)]
    pub(super) bytes_sent: u64,
    pub(super) bytes_received: u64,
    pub(super) total_size: u64,
}

/// Result of a single benchmark run.
#[derive(Clone, Debug)]
pub(super) struct BenchmarkResult {
    pub(super) version: String,
    pub(super) runs: Vec<Duration>,
    pub(super) samples: Vec<RunSample>,
    pub(super) mean: Duration,
    pub(super) min: Duration,
    pub(super) max: Duration,
    pub(super) stddev: f64,
    pub(super) mean_throughput_mbps: f64,
}

impl BenchmarkResult {
    pub(super) fn new(version: String, samples: Vec<RunSample>) -> Self {
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

/// Checks if rsync daemon is running on the given port.
pub(super) fn check_daemon_running(port: u16) -> bool {
    use std::net::TcpStream;
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
            let _ = Command::new("kill").arg(pid.to_string()).status();
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = fs::remove_file(&pid_path);
}

/// Starts an rsync daemon for benchmarking.
pub(super) fn start_daemon(workspace: &Path, options: &BenchmarkOptions) -> TaskResult<()> {
    stop_daemon(options);

    let conf_path = options.bench_dir.join("rsyncd.conf");
    let pid_path = options.bench_dir.join("rsyncd.pid");
    let log_path = options.bench_dir.join("rsyncd.log");

    let kernel_dir = options.bench_dir.join("kernel-src");
    if !kernel_dir.exists() {
        println!("Downloading Linux kernel source for benchmarking...");
        download_kernel_source(&options.bench_dir)?;
    }

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

    std::thread::sleep(Duration::from_millis(500));

    Ok(())
}

/// Downloads Linux kernel source for benchmarking.
fn download_kernel_source(bench_dir: &Path) -> TaskResult<()> {
    let tarball = bench_dir.join("linux-6.12.tar.xz");
    let kernel_dir = bench_dir.join("kernel-src");

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
pub(super) fn detect_versions(workspace: &Path) -> TaskResult<Vec<String>> {
    let output = Command::new("git")
        .args(["tag", "-l", "v0.*", "--sort=-v:refname"])
        .current_dir(workspace)
        .output()?;

    let tags: Vec<String> = BufReader::new(&output.stdout[..])
        .lines()
        .map_while(|l| l.ok())
        .take(3)
        .collect();

    Ok(tags)
}

/// Finds the upstream rsync binary.
pub(super) fn find_upstream_rsync() -> Option<PathBuf> {
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
pub(super) fn build_release(workspace: &Path) -> TaskResult<()> {
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
pub(super) fn build_version(workspace: &Path, version: &str, output: &Path) -> TaskResult<()> {
    let worktree_dir = workspace.join("target").join(format!("worktree-{version}"));

    let _ = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_dir.to_str().unwrap(),
        ])
        .current_dir(workspace)
        .status();

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

    let binary = worktree_dir.join("target/release/oc-rsync");
    fs::copy(&binary, output)?;

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
pub(super) fn run_benchmark(
    binary: &Path,
    source_url: &str,
    dest_dir: &Path,
    runs: usize,
) -> TaskResult<Vec<RunSample>> {
    let mut results = Vec::with_capacity(runs);

    for i in 0..runs {
        let _ = fs::remove_dir_all(dest_dir);
        fs::create_dir_all(dest_dir)?;

        // Best-effort cache drop before timing.
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
pub(super) fn parse_stats_output(output: &str, elapsed: Duration) -> RunSample {
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
///
/// Input is typically "  1,234,567 bytes" or "  1,234,567".
pub(super) fn parse_stat_value(s: &str) -> u64 {
    s.split_whitespace()
        .next()
        .unwrap_or("0")
        .replace(',', "")
        .parse()
        .unwrap_or(0)
}
