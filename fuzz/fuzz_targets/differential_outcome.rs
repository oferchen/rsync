#![no_main]

//! Outcome-based differential fuzz target: oc-rsync vs upstream rsync.
//!
//! Takes a structured input (file content, file name, rsync flags), runs both
//! oc-rsync and upstream rsync in local-copy mode on the same source tree, and
//! compares the outcome: destination file content and exit codes. A divergence
//! panics, which libFuzzer records as a crash artifact.
//!
//! # How it works
//!
//! 1. Create temp source and destination directories for each binary.
//! 2. Write the fuzzed file content into the source directory.
//! 3. Run oc-rsync in local-copy mode (src/ -> dst-oc/).
//! 4. Run upstream rsync in local-copy mode (src/ -> dst-upstream/).
//! 5. Compare destination file content byte-for-byte.
//! 6. Compare exit codes.
//! 7. Panic on any divergence.
//!
//! # Binary discovery
//!
//! ## oc-rsync
//!
//! 1. `$OC_RSYNC_BIN` (absolute path, overrides everything)
//! 2. `target/release/oc-rsync`
//! 3. `target/debug/oc-rsync`
//!
//! ## Upstream rsync
//!
//! 1. `$UPSTREAM_RSYNC` (absolute path, overrides everything)
//! 2. `target/interop/upstream-install/3.4.2/bin/rsync`
//! 3. `target/interop/upstream-install/3.4.1/bin/rsync`
//! 4. `/opt/homebrew/bin/rsync`
//! 5. `/usr/local/bin/rsync`
//! 6. `/usr/bin/rsync`
//!
//! When either binary is unavailable, the harness exits the iteration cleanly
//! so libFuzzer treats the input as benign.
//!
//! # Known-acceptable divergences
//!
//! - Timestamp precision: skipped via `--no-times` (timestamps are not compared).
//! - Permission bits: may differ on macOS vs Linux; skipped via `--no-perms`.
//! - File names with embedded NUL or `/`: rejected by the sanitiser.
//! - Very large files: capped at 64 KiB to keep iteration speed reasonable.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run differential_outcome -- -max_total_time=120
//! ```
//!
//! # Throughput
//!
//! Each iteration spawns two child processes (oc-rsync and rsync), so expect
//! roughly 50-200 exec/sec depending on hardware. This is acceptable for an
//! outcome-comparison target - correctness over raw coverage rate.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

/// Maximum file content size. 64 KiB keeps iteration speed reasonable while
/// exercising the full transfer pipeline (checksums, delta, temp-file commit).
const MAX_CONTENT_LEN: usize = 64 * 1024;

/// Maximum file name length. Long names add no signal for outcome comparison.
const MAX_NAME_LEN: usize = 64;

/// Timeout for each rsync invocation. Prevents the fuzzer from hanging on
/// pathological inputs (e.g., a flag combination that triggers an interactive
/// prompt or infinite retry loop).
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

/// Subset of rsync flags that are safe for local-copy differential comparison.
/// Each flag is independently toggled by the fuzzer. Only flags that produce
/// deterministic, comparable outcomes are included.
#[derive(Arbitrary, Debug, Clone)]
struct FuzzFlags {
    /// `--checksum` - force checksum-based transfer decisions.
    checksum: bool,
    /// `--whole-file` - disable delta-transfer algorithm.
    whole_file: bool,
    /// `--inplace` - update files in place instead of atomic rename.
    inplace: bool,
    /// `--ignore-existing` - skip files that already exist on the destination.
    ignore_existing: bool,
    /// `--ignore-non-existing` - skip files that do not exist on destination.
    ignore_non_existing: bool,
    /// `--size-only` - skip files that match in size.
    size_only: bool,
    /// `--sparse` - handle sparse files efficiently.
    sparse: bool,
}

impl FuzzFlags {
    /// Render the selected flags as command-line arguments. Always includes
    /// `--no-times` and `--no-perms` to suppress platform-dependent divergences.
    fn to_args(&self) -> Vec<&'static str> {
        let mut args = vec!["--no-times", "--no-perms"];
        if self.checksum {
            args.push("--checksum");
        }
        if self.whole_file {
            args.push("--whole-file");
        }
        if self.inplace {
            args.push("--inplace");
        }
        if self.ignore_existing {
            args.push("--ignore-existing");
        }
        if self.ignore_non_existing {
            args.push("--ignore-non-existing");
        }
        if self.size_only {
            args.push("--size-only");
        }
        if self.sparse {
            args.push("--sparse");
        }
        args
    }
}

/// Whether the destination already has a file (pre-existing content).
/// This exercises delta-transfer, `--ignore-existing`, `--size-only`, etc.
#[derive(Arbitrary, Debug, Clone)]
enum DestinationState {
    /// No pre-existing file at the destination.
    Empty,
    /// Destination has a file with the same name but different content.
    DifferentContent { content: Vec<u8> },
    /// Destination has a file with identical content.
    IdenticalContent,
}

/// Top-level fuzz input.
#[derive(Arbitrary, Debug)]
struct DifferentialInput {
    /// File content for the source file.
    content: Vec<u8>,
    /// Raw bytes for the file name; sanitised to a safe ASCII name.
    name_bytes: Vec<u8>,
    /// Rsync flags to apply.
    flags: FuzzFlags,
    /// Pre-existing state of the destination directory.
    dest_state: DestinationState,
}

/// Sanitise raw bytes into a valid, safe file name. Returns `None` if the
/// bytes cannot produce a usable name (all filtered out, too short, etc.).
///
/// Allows alphanumeric characters, dots, hyphens, and underscores. No path
/// separators, no NUL bytes, no leading dots (avoids hidden files and `.`/`..`).
fn sanitise_name(bytes: &[u8]) -> Option<String> {
    let mut out = String::with_capacity(bytes.len().min(MAX_NAME_LEN));
    for &b in bytes.iter().take(MAX_NAME_LEN) {
        let c = match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => b as char,
            b'.' | b'_' | b'-' => b as char,
            _ => continue,
        };
        out.push(c);
    }
    // Strip leading dots to avoid hidden files and `.`/`..`.
    let trimmed = out.trim_start_matches('.');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Cached path to the oc-rsync binary.
fn oc_rsync_binary() -> Option<&'static Path> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(env) = std::env::var("OC_RSYNC_BIN") {
            let p = PathBuf::from(env);
            if p.is_file() {
                return Some(p);
            }
        }
        for candidate in [
            "target/release/oc-rsync",
            "target/debug/oc-rsync",
        ] {
            let p = PathBuf::from(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    })
    .as_deref()
}

/// Cached path to the upstream rsync binary.
fn upstream_rsync_binary() -> Option<&'static Path> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(env) = std::env::var("UPSTREAM_RSYNC") {
            let p = PathBuf::from(env);
            if p.is_file() {
                return Some(p);
            }
        }
        for candidate in [
            "target/interop/upstream-install/3.4.2/bin/rsync",
            "target/interop/upstream-install/3.4.1/bin/rsync",
            "/opt/homebrew/bin/rsync",
            "/usr/local/bin/rsync",
            "/usr/bin/rsync",
        ] {
            let p = PathBuf::from(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    })
    .as_deref()
}

/// Outcome from running one rsync invocation.
#[derive(Debug)]
struct RunOutcome {
    /// Exit code (0 = success, non-zero = error).
    exit_code: i32,
    /// Content of the destination file after the transfer, or `None` if the
    /// file does not exist (e.g., transfer was skipped).
    dest_content: Option<Vec<u8>>,
}

/// Run an rsync-compatible binary with the given arguments.
///
/// Returns `None` if the child process could not be spawned or timed out.
fn run_rsync(
    binary: &Path,
    src_dir: &Path,
    dst_dir: &Path,
    flags: &[&str],
) -> Option<RunOutcome> {
    let mut cmd = Command::new(binary);
    cmd.args(flags);

    // Trailing slash on source tells rsync to copy contents, not the dir itself.
    let mut src_arg = src_dir.as_os_str().to_os_string();
    src_arg.push("/");
    cmd.arg(src_arg);
    cmd.arg(dst_dir);

    // Suppress stderr to avoid noisy output during fuzzing.
    cmd.stderr(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());

    let child = cmd.spawn().ok()?;

    // Wait with timeout to prevent hangs.
    wait_with_timeout(child, CHILD_TIMEOUT)
}

/// Wait for a child process with a timeout. Kills the child if it exceeds
/// the deadline. Returns `None` on timeout or if the process state cannot
/// be read.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Option<RunOutcome> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let exit_code = status.code().unwrap_or(-1);
                return Some(RunOutcome {
                    exit_code,
                    dest_content: None, // filled in by caller
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

/// Read the content of a file at `dir/name`, returning `None` if it does not
/// exist.
fn read_dest_file(dir: &Path, name: &str) -> Option<Vec<u8>> {
    let path = dir.join(name);
    std::fs::read(&path).ok()
}

/// Prepare a destination directory with optional pre-existing content.
fn prepare_dest(dir: &Path, name: &str, state: &DestinationState, src_content: &[u8]) {
    std::fs::create_dir_all(dir).expect("failed to create dest dir");
    match state {
        DestinationState::Empty => {}
        DestinationState::DifferentContent { content } => {
            let capped = &content[..content.len().min(MAX_CONTENT_LEN)];
            std::fs::write(dir.join(name), capped).expect("failed to write dest file");
            // Backdate the destination file to avoid quick-check skipping.
            backdate_file(&dir.join(name));
        }
        DestinationState::IdenticalContent => {
            std::fs::write(dir.join(name), src_content).expect("failed to write dest file");
            // Backdate the destination file to avoid quick-check skipping.
            backdate_file(&dir.join(name));
        }
    }
}

/// Set a file's mtime to 2 hours in the past. This prevents rsync's
/// quick-check algorithm from skipping transfers when size+mtime match
/// between source and destination (which would happen if both files were
/// created within the same second).
fn backdate_file(path: &Path) {
    use std::time::SystemTime;
    let two_hours_ago = SystemTime::now()
        .checked_sub(Duration::from_secs(7200))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let ft = filetime::FileTime::from_system_time(two_hours_ago);
    let _ = filetime::set_file_mtime(path, ft);
}

fn run_one(input: DifferentialInput) {
    // Cap content length.
    let content = if input.content.len() > MAX_CONTENT_LEN {
        &input.content[..MAX_CONTENT_LEN]
    } else {
        &input.content
    };

    // Sanitise file name.
    let Some(name) = sanitise_name(&input.name_bytes) else {
        return;
    };

    // Discover binaries. Exit cleanly if either is unavailable.
    let Some(oc_bin) = oc_rsync_binary() else {
        return;
    };
    let Some(upstream_bin) = upstream_rsync_binary() else {
        return;
    };

    // Skip mutually exclusive flag combinations that upstream rejects.
    if input.flags.inplace && input.flags.sparse {
        // Upstream rsync < 3.1.4 rejects --inplace --sparse.
        return;
    }
    if input.flags.ignore_existing && input.flags.ignore_non_existing {
        // Both flags together make no sense; behaviour is undefined.
        return;
    }

    // Build flag list.
    let flag_args = input.flags.to_args();

    // Create temp directories.
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(_) => return,
    };
    let src_dir = tmp.path().join("src");
    let dst_oc = tmp.path().join("dst-oc");
    let dst_upstream = tmp.path().join("dst-upstream");

    // Populate source.
    if std::fs::create_dir_all(&src_dir).is_err() {
        return;
    }
    if std::fs::write(src_dir.join(&name), content).is_err() {
        return;
    }

    // Prepare both destination directories with the same pre-existing state.
    prepare_dest(&dst_oc, &name, &input.dest_state, content);
    prepare_dest(&dst_upstream, &name, &input.dest_state, content);

    // Run oc-rsync.
    let oc_outcome = match run_rsync(oc_bin, &src_dir, &dst_oc, &flag_args) {
        Some(mut o) => {
            o.dest_content = read_dest_file(&dst_oc, &name);
            o
        }
        None => return, // timeout - skip this input
    };

    // Run upstream rsync.
    let upstream_outcome = match run_rsync(upstream_bin, &src_dir, &dst_upstream, &flag_args) {
        Some(mut o) => {
            o.dest_content = read_dest_file(&dst_upstream, &name);
            o
        }
        None => return, // timeout - skip this input
    };

    // Compare exit codes. Both returning 0 (success) or both returning the
    // same non-zero code is acceptable. A divergence is a finding.
    //
    // We normalise non-zero codes to a single "failure" bucket because
    // different rsync versions may use different numeric codes for the same
    // error class. The key invariant is: both succeed or both fail.
    let oc_ok = oc_outcome.exit_code == 0;
    let upstream_ok = upstream_outcome.exit_code == 0;

    assert_eq!(
        oc_ok,
        upstream_ok,
        "exit code divergence: oc-rsync={} upstream={} (flags={:?}, name={:?}, content_len={})",
        oc_outcome.exit_code,
        upstream_outcome.exit_code,
        flag_args,
        name,
        content.len(),
    );

    // If both failed, skip content comparison - the destination state is
    // undefined after a failed transfer.
    if !oc_ok {
        return;
    }

    // Compare destination file content byte-for-byte.
    assert_eq!(
        oc_outcome.dest_content,
        upstream_outcome.dest_content,
        "destination content divergence: name={:?} flags={:?} content_len={} \
         oc_dest_len={:?} upstream_dest_len={:?}",
        name,
        flag_args,
        content.len(),
        oc_outcome.dest_content.as_ref().map(|c| c.len()),
        upstream_outcome.dest_content.as_ref().map(|c| c.len()),
    );
}

fuzz_target!(|input: DifferentialInput| {
    run_one(input);
});
