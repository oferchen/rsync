//! Many-small-files loopback benchmark across transports, plus a local I/O
//! technology sweep.
//!
//! Generates a per-file-overhead workload (default 10k files, scales cleanly to
//! 100k) and times oc-rsync against upstream as the pulling client over each
//! transport, taking the median wall time across a few runs. It then sweeps
//! oc-rsync's local-copy I/O backends (io_uring, copy_file_range / reflink,
//! parallel checksum, whole-file) and confirms which backend was exercised via
//! oc's `--debug=IOURING,CLONE` tracing. The numbers feed performance work;
//! this is opt-in via `--bench`.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Instant;

use crate::commands::validate::ValidateCtx;
use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::error::{TaskError, TaskResult};

/// Timed runs per configuration; the median is reported.
const RUNS: usize = 3;
/// Files per generated subdirectory.
const PER_DIR: usize = 100;

/// One oc-rsync I/O backend configuration for the local sweep.
///
/// Every entry uses the same base flags (`-a`); configurations differ only by
/// runtime environment toggles and/or an extra flag, so a timing difference is
/// attributable to the I/O path rather than the transfer semantics.
struct IoConfig {
    /// Short label for the sweep row.
    label: &'static str,
    /// Environment toggles set on the child process.
    env: &'static [(&'static str, &'static str)],
    /// Extra flags appended after `-a`.
    extra_flags: &'static [&'static str],
}

/// The I/O backends swept on the local transport.
///
/// - `default`: auto io_uring on Linux 5.6+, standard elsewhere.
/// - `no-iouring`: force the standard read/write path.
/// - `iouring-data-writes`: opt into registered-buffer data writes.
/// - `parallel-checksum`: parallelize basis checksum computation.
/// - `whole-file`: `-W`, skip the delta algorithm entirely.
const IO_CONFIGS: &[IoConfig] = &[
    IoConfig {
        label: "default",
        env: &[],
        extra_flags: &[],
    },
    IoConfig {
        label: "no-iouring",
        env: &[("OC_RSYNC_DISABLE_IOURING", "1")],
        extra_flags: &[],
    },
    IoConfig {
        label: "iouring-data-writes",
        env: &[("OC_RSYNC_IOURING_DATA_WRITES", "1")],
        extra_flags: &[],
    },
    IoConfig {
        label: "parallel-checksum",
        env: &[("OC_RSYNC_PARALLEL_CHECKSUM", "1")],
        extra_flags: &[],
    },
    IoConfig {
        label: "whole-file",
        env: &[],
        extra_flags: &["-W"],
    },
];

/// Run the benchmark and print a per-transport comparison table plus the local
/// I/O technology sweep.
pub fn run(ctx: &ValidateCtx, files: usize) -> TaskResult<()> {
    let root = ctx.work.join("bench");
    let src = root.join("src");
    let bytes = generate(&src, files)?;
    let flags: Vec<String> = ["-a"].iter().map(|s| s.to_string()).collect();

    eprintln!(
        "\n=== benchmark: {files} files, {:.1} MB, median of {RUNS} ===",
        bytes as f64 / 1_048_576.0
    );
    for &transport in ctx.transports {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            eprintln!("  {label:<14}  SKIP (no sshd on localhost:22)");
            continue;
        }
        let oc = median_secs(transport, ctx.oc, ctx, &src, &root.join("oc"), &flags)?;
        // Upstream has no russh client; time it over its ssh-subprocess equivalent.
        let up = median_secs(
            transport.for_upstream(),
            ctx.upstream,
            ctx,
            &src,
            &root.join("up"),
            &flags,
        )?;
        let speedup = if oc > 0.0 { up / oc } else { 0.0 };
        eprintln!("  {label:<14}  oc {oc:6.3}s   upstream {up:6.3}s   ({speedup:.2}x)");
    }

    io_sweep(ctx.oc, &src, &root.join("sweep"))?;
    Ok(())
}

/// Median wall time of `RUNS` full pulls with `client` over `transport`.
fn median_secs(
    transport: Transport,
    client: &Path,
    ctx: &ValidateCtx,
    src: &Path,
    dst: &Path,
    flags: &[String],
) -> TaskResult<f64> {
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let start = Instant::now();
        let out = pull_into(transport, client, ctx.upstream, src, dst, flags, ctx.work)?;
        if !out.status.success() {
            return Err(TaskError::Validation(format!(
                "benchmark transfer failed on {}: {}",
                transport.label(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        times.push(start.elapsed().as_secs_f64());
    }
    Ok(median(times))
}

/// Sweep oc-rsync's local-copy I/O backends: time each configuration and derive
/// a backend note from a debug-traced run that confirms which path ran.
///
/// io_uring is Linux-only, so the sweep is skipped with a note elsewhere.
fn io_sweep(oc: &Path, src: &Path, dst: &Path) -> TaskResult<()> {
    eprintln!("\n=== I/O technology sweep (local, oc-rsync) ===");
    if !cfg!(target_os = "linux") {
        eprintln!("  SKIP (io_uring is Linux-only; run on a Linux host)");
        return Ok(());
    }
    for config in IO_CONFIGS {
        let secs = sweep_median(oc, config, src, dst)?;
        let note = confirm_backend(oc, config, src, dst)?;
        eprintln!("  {:<20} {secs:6.3}s   {note}", config.label);
    }
    Ok(())
}

/// Median wall time of `RUNS` fresh local pulls under one I/O configuration.
///
/// The destination is reset before each run so every timing exercises a full
/// copy through the local-copy executor rather than a quick-check skip.
fn sweep_median(oc: &Path, config: &IoConfig, src: &Path, dst: &Path) -> TaskResult<f64> {
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        reset_dir(dst)?;
        let start = Instant::now();
        let out = run_local(oc, config, src, dst, &[])?;
        if !out.status.success() {
            return Err(TaskError::Validation(format!(
                "sweep transfer failed for `{}`: {}",
                config.label,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        times.push(start.elapsed().as_secs_f64());
    }
    Ok(median(times))
}

/// Run one extra debug-traced pull and summarize which backend it exercised.
fn confirm_backend(oc: &Path, config: &IoConfig, src: &Path, dst: &Path) -> TaskResult<String> {
    reset_dir(dst)?;
    let out = run_local(oc, config, src, dst, &["--debug=IOURING,CLONE"])?;
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push('\n');
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(backend_note(&combined))
}

/// Spawn a local oc-rsync pull of `src/` into `dst/` under `config`.
///
/// `extra` carries diagnostic flags (e.g. `--debug=...`) that must not appear in
/// timed runs. Environment toggles are applied to the child only, so they never
/// leak into the parent xtask process.
fn run_local(
    oc: &Path,
    config: &IoConfig,
    src: &Path,
    dst: &Path,
    extra: &[&str],
) -> TaskResult<Output> {
    let mut cmd = Command::new(oc);
    cmd.arg("-a");
    cmd.args(config.extra_flags);
    cmd.args(extra);
    for &(key, value) in config.env {
        cmd.env(key, value);
    }
    cmd.arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()));
    cmd.output()
        .map_err(|e| TaskError::Validation(format!("spawn sweep `{}`: {e}", config.label)))
}

/// Derive a one-line backend note from `--debug=IOURING,CLONE` output.
///
/// Counts trace lines that indicate an active io_uring dispatch and lines that
/// indicate a CoW clone path (`copy_file_range` / `clone` / `reflink`), ignoring
/// lines that report a path was unavailable. The result reads like
/// `io_uring=N clone=M`, or `standard (no io_uring)` when no io_uring op ran.
fn backend_note(debug_output: &str) -> String {
    let mut iouring = 0usize;
    let mut clone = 0usize;
    for line in debug_output.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("unavailable") {
            continue;
        }
        if lower.contains("uring") {
            iouring += 1;
        }
        if lower.contains("copy_file_range") || lower.contains("clone") || lower.contains("reflink")
        {
            clone += 1;
        }
    }
    match (iouring, clone) {
        (0, 0) => "standard (no io_uring)".to_string(),
        (0, m) => format!("standard (no io_uring), clone={m}"),
        (n, m) => format!("io_uring={n} clone={m}"),
    }
}

/// Median of `times`; the lower-middle element for even counts.
fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    times[times.len() / 2]
}

/// Recreate `dir` as an empty directory.
fn reset_dir(dir: &Path) -> TaskResult<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| TaskError::Validation(format!("remove {}: {e}", dir.display())))?;
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| TaskError::Validation(format!("create {}: {e}", dir.display())))
}

/// Generate `files` files across subdirectories with varied sizes; return the
/// total byte count.
fn generate(src: &Path, files: usize) -> TaskResult<u64> {
    if src.exists() {
        std::fs::remove_dir_all(src)
            .map_err(|e| TaskError::Validation(format!("clean bench src: {e}")))?;
    }
    let filler = vec![b'x'; 512 * 1024];
    let mut total = 0u64;
    for index in 0..files {
        if index % PER_DIR == 0 {
            std::fs::create_dir_all(src.join(format!("d{:04}", index / PER_DIR)))
                .map_err(|e| TaskError::Validation(format!("create bench dir: {e}")))?;
        }
        let size = match index % 50 {
            0 => 256 * 1024,
            1 | 2 => 64 * 1024,
            _ => 1024 + (index % 3072),
        };
        let path = src
            .join(format!("d{:04}", index / PER_DIR))
            .join(format!("f{index:05}.dat"));
        std::fs::write(&path, &filler[..size])
            .map_err(|e| TaskError::Validation(format!("write bench file: {e}")))?;
        total += size as u64;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::{backend_note, median};

    #[test]
    fn backend_note_reports_iouring_and_clone_counts() {
        let output = "\
io_uring available: cached probe reports supported
CoW clone succeeded: dst=d0000/f0.dat (262144 bytes)
cloned d0000/f1.dat: 262144 bytes (FICLONE)";
        assert_eq!(backend_note(output), "io_uring=1 clone=2");
    }

    #[test]
    fn backend_note_reports_standard_when_iouring_disabled() {
        // A disabled probe emits an "unavailable" line, which must not count as
        // an active io_uring op - this is exactly the `no-iouring` toggle proof.
        let output = "io_uring unavailable: disabled by OC_RSYNC_DISABLE_IOURING";
        assert_eq!(backend_note(output), "standard (no io_uring)");
    }

    #[test]
    fn backend_note_reports_clone_only_without_iouring() {
        let output = "\
io_uring unavailable: disabled by OC_RSYNC_DISABLE_IOURING
CoW clone succeeded: dst=d0000/f0.dat (262144 bytes)";
        assert_eq!(backend_note(output), "standard (no io_uring), clone=1");
    }

    #[test]
    fn backend_note_is_standard_on_empty_output() {
        assert_eq!(backend_note(""), "standard (no io_uring)");
    }

    #[test]
    fn median_picks_the_middle_element() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(vec![5.0]), 5.0);
        // Even count: the lower-middle element after sorting.
        assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), 3.0);
    }
}
