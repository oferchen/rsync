//! Many-small-files loopback benchmark across transports.
//!
//! Generates a per-file-overhead workload (default 10k files) and times
//! oc-rsync against upstream as the pulling client over each transport, taking
//! the median wall time across a few runs. The numbers feed performance work;
//! this is opt-in via `--bench`.

use std::path::Path;
use std::time::Instant;

use crate::commands::validate::ValidateCtx;
use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::error::{TaskError, TaskResult};

/// Timed runs per (transport, client); the median is reported.
const RUNS: usize = 3;
/// Files per generated subdirectory.
const PER_DIR: usize = 100;

/// Run the benchmark and print a per-transport comparison table.
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
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(times[times.len() / 2])
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
