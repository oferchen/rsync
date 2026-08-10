//! SSR-1: SSH push throughput regression test vs upstream rsync baseline.
//!
//! # Why this test exists
//!
//! oc-rsync v0.6.1 shipped a subprocess-based SSH push path whose
//! goodbye-phase handshake deadlocked against the remote, making SSH
//! pushes roughly **200x slower** than upstream rsync. v0.6.2 fixed the
//! regression by switching the SSH push path to a russh-backed
//! implementation. SSR-1 is the regression-asserting counterpart to
//! SSR-5's CI smoke bench: where SSR-5 reports timings as a non-required
//! check, SSR-1 raises a `panic!` (test failure) the moment oc-rsync SSH
//! push wall-clock exceeds a fixed multiple of upstream's.
//!
//! # Threshold rationale (2x)
//!
//! The historical bug was a 200x regression. A 2x ceiling is deliberately
//! generous: it tolerates a substantial slowdown across heterogeneous CI
//! runners and OpenSSH versions while still detecting catastrophic
//! regressions of the v0.6.1 class by two orders of magnitude. SSR-5
//! polices the tighter 1.5x ratio as an advisory signal; SSR-1 is the
//! hard backstop and runs nightly via `--run-ignored=only`.
//!
//! # Related cells
//!
//! - SSR-2: russh-backed SSH push path (the fix).
//! - SSR-3: SSH push regression interop matrix.
//! - SSR-4: SSH push goodbye-phase fuzz / handshake parity.
//! - SSR-5: hyperfine-based SSH push smoke bench (advisory timings,
//!   1.5x ratio, `scripts/ci/ssh_push_smoke_bench.sh` +
//!   `.github/workflows/ssh-smoke-bench.yml`).
//!
//! # Design
//!
//! For each of three file-size buckets (1 KB, 1 MB, 100 MB - mirroring
//! SSR-5) the test:
//!
//! 1. Generates a reproducible pseudo-random payload from a fixed seed.
//! 2. Times an oc-rsync SSH push to `localhost` three times and takes the
//!    median wall-clock.
//! 3. Times the equivalent upstream rsync SSH push three times and takes
//!    the median wall-clock.
//! 4. Computes `ratio = oc_median / upstream_median` and asserts it is
//!    below 2.0.
//!
//! # Skip conditions
//!
//! The test exits early with a diagnostic `eprintln!` (no failure) when:
//! - The platform is not Unix (gated at the crate level via `#![cfg(unix)]`).
//! - Upstream `rsync` is not on PATH (or `UPSTREAM_RSYNC` is unset/missing).
//! - SSH to localhost cannot complete in batch mode within a 3 s
//!   connect-timeout (no sshd, no key-based auth, no `authorized_keys`).
//! - The oc-rsync binary cannot be located under `target/{debug,release,dist}/`.
//! - Generating a per-bucket payload fails (e.g. tmpfs short on space).
//!
//! # Cost and scheduling
//!
//! The 100 MB bucket dominates runtime: roughly 30 s for 3 runs of each
//! binary on a typical CI runner. To keep the default nextest cycle
//! fast the test is marked `#[ignore]`; CI's nightly cell opts in with
//! `nextest run --run-ignored=only -E 'test(ssh_push_throughput_regression_ssr1)'`.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Hard ceiling on `oc_median / upstream_median` per size bucket.
///
/// See the file-level doc comment for the 2x rationale: it is two orders
/// of magnitude below the v0.6.1 incident (~200x) while still tolerating
/// CI-runner noise and OpenSSH-version variance.
const RATIO_LIMIT: f64 = 2.0;

/// Per-bucket repetitions. The median of three eliminates a single
/// runaway outlier (GC pause, sshd cold start) without inflating cost.
const RUNS_PER_BUCKET: usize = 3;

/// Wall-clock cap per individual SSH push invocation. 100 MB across SSH
/// loopback completes in well under a minute on any sane runner; anything
/// past 120 s is a deadlock symptom.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Connect-timeout for the loopback SSH availability probe.
const SSH_PROBE_TIMEOUT_SECS: u64 = 3;

/// File-size buckets in bytes, matching SSR-5 exactly so the two cells
/// stay comparable across releases.
const BUCKETS: &[(&str, usize)] = &[
    ("1KB", 1024),
    ("1MB", 1024 * 1024),
    ("100MB", 100 * 1024 * 1024),
];

/// Resolve the oc-rsync binary path the same way other root-level tests do.
fn oc_rsync_binary() -> Option<PathBuf> {
    if let Some(env_path) = std::env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    for profile in ["release", "dist", "debug"] {
        let path = PathBuf::from(manifest_dir)
            .join("target")
            .join(profile)
            .join("oc-rsync");
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Resolve the upstream rsync binary. Honours `UPSTREAM_RSYNC` when set,
/// otherwise falls back to whatever `rsync` resolves to on PATH.
fn upstream_rsync_binary() -> Option<PathBuf> {
    if let Some(env_path) = std::env::var_os("UPSTREAM_RSYNC") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Some(path);
        }
    }
    let probe = Command::new("rsync").arg("--version").output().ok()?;
    probe.status.success().then(|| PathBuf::from("rsync"))
}

/// Resolve the SSH target (default `$USER@localhost`).
fn ssh_target() -> Option<String> {
    if let Ok(target) = std::env::var("SSH_TARGET") {
        if !target.is_empty() {
            return Some(target);
        }
    }
    let user = std::env::var("USER").ok()?;
    if user.is_empty() {
        return None;
    }
    Some(format!("{user}@localhost"))
}

/// Probe loopback SSH with the same options used by the timed transfers.
fn ssh_localhost_available(target: &str) -> bool {
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            &format!("ConnectTimeout={SSH_PROBE_TIMEOUT_SECS}"),
            "-o",
            "StrictHostKeyChecking=no",
            target,
            "true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate a reproducible random payload of `bytes` bytes at `path`.
fn write_payload(path: &Path, bytes: usize) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(path)?;
    let mut remaining = bytes;
    // 64 KiB pseudo-random chunk derived from a fixed seed mixed with the
    // chunk index. Avoiding /dev/urandom keeps the test self-contained and
    // deterministic, which makes flake triage tractable.
    let mut chunk_idx: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15u64.wrapping_add(chunk_idx);
        for slot in buf.iter_mut() {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *slot = (seed >> 33) as u8;
        }
        let take = remaining.min(buf.len());
        file.write_all(&buf[..take])?;
        remaining -= take;
        chunk_idx = chunk_idx.wrapping_add(1);
    }
    file.sync_all()?;
    Ok(())
}

/// Reset `dst` so the next push is a cold transfer (no quick-check skip).
fn reset_destination(dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    fs::create_dir_all(dst)
}

/// Run one timed SSH push and return the wall-clock duration. Returns
/// `None` if the process times out or exits non-zero (treated as a hard
/// failure by the caller).
fn time_one_push(binary: &Path, src: &Path, target: &str, dst: &Path) -> Option<Duration> {
    reset_destination(dst).ok()?;
    let dst_spec = format!("{}:{}/", target, dst.display());
    let mut child = Command::new(binary)
        .arg("-a")
        .arg("--rsh")
        .arg(format!("ssh -o BatchMode=yes -o StrictHostKeyChecking=no -o ConnectTimeout={SSH_PROBE_TIMEOUT_SECS}"))
        .arg(src)
        .arg(&dst_spec)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let start = Instant::now();
    let deadline = start + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let elapsed = start.elapsed();
                if !status.success() {
                    let mut stderr = String::new();
                    if let Some(mut s) = child.stderr.take() {
                        use std::io::Read;
                        let _ = s.read_to_string(&mut stderr);
                    }
                    eprintln!(
                        "    push failed: binary={} status={:?} stderr={}",
                        binary.display(),
                        status.code(),
                        stderr.trim()
                    );
                    return None;
                }
                return Some(elapsed);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!(
                        "    push timed out after {:?}: binary={}",
                        RUN_TIMEOUT,
                        binary.display()
                    );
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

/// Time `RUNS_PER_BUCKET` pushes and return the median duration, or
/// `None` if any individual run failed.
fn median_push(binary: &Path, src: &Path, target: &str, dst: &Path) -> Option<Duration> {
    let mut samples = Vec::with_capacity(RUNS_PER_BUCKET);
    for run in 0..RUNS_PER_BUCKET {
        let t = time_one_push(binary, src, target, dst)?;
        eprintln!("    run {}: {:?}", run + 1, t);
        samples.push(t);
    }
    samples.sort();
    samples.get(samples.len() / 2).copied()
}

#[test]
#[ignore = "expensive (100MB SSH loopback); runs in nightly --run-ignored=only cell"]
fn ssh_push_throughput_regression_ssr1() {
    let Some(oc_rsync) = oc_rsync_binary() else {
        eprintln!("SSR-1 skip: oc-rsync binary not found under target/{{release,dist,debug}}");
        return;
    };
    let Some(upstream) = upstream_rsync_binary() else {
        eprintln!("SSR-1 skip: upstream rsync not on PATH (set UPSTREAM_RSYNC to override)");
        return;
    };
    let Some(target) = ssh_target() else {
        eprintln!("SSR-1 skip: cannot derive SSH target (set SSH_TARGET=user@host)");
        return;
    };
    if !ssh_localhost_available(&target) {
        eprintln!("SSR-1 skip: ssh -o BatchMode=yes {target} true failed (no sshd / no keys)");
        return;
    }

    let work = TempDir::new().expect("create work tempdir");
    eprintln!(
        "SSR-1: oc-rsync={} upstream={} target={} buckets={} runs={} ratio_limit={}",
        oc_rsync.display(),
        upstream.display(),
        target,
        BUCKETS.len(),
        RUNS_PER_BUCKET,
        RATIO_LIMIT,
    );

    for (label, bytes) in BUCKETS {
        eprintln!("SSR-1 bucket {label} ({bytes} bytes)");
        let src = work.path().join(format!("src-{label}")).join("payload.bin");
        if let Err(err) = write_payload(&src, *bytes) {
            eprintln!("SSR-1 skip bucket {label}: write payload failed: {err}");
            continue;
        }
        let dst_oc = work.path().join(format!("dst-oc-{label}"));
        let dst_up = work.path().join(format!("dst-up-{label}"));

        eprintln!("  oc-rsync runs:");
        let Some(oc_median) = median_push(&oc_rsync, &src, &target, &dst_oc) else {
            eprintln!("SSR-1 skip bucket {label}: oc-rsync push failed");
            continue;
        };
        eprintln!("  upstream runs:");
        let Some(up_median) = median_push(&upstream, &src, &target, &dst_up) else {
            eprintln!("SSR-1 skip bucket {label}: upstream rsync push failed");
            continue;
        };

        let oc_secs = oc_median.as_secs_f64();
        let up_secs = up_median.as_secs_f64();
        let ratio = if up_secs > 0.0 {
            oc_secs / up_secs
        } else {
            f64::INFINITY
        };
        eprintln!(
            "  result {label}: oc={oc_secs:.4}s upstream={up_secs:.4}s ratio={ratio:.3}x limit={RATIO_LIMIT}x"
        );

        assert!(
            ratio < RATIO_LIMIT,
            "SSR-1 {label}: oc-rsync SSH push is {ratio:.1}x slower than upstream ({oc_secs:.4}s vs {up_secs:.4}s) at {bytes} bytes - exceeds {RATIO_LIMIT}x ceiling (v0.6.1 regression was ~200x)",
        );
    }
}
