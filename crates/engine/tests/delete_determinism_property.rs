//! Property tests for the parallel-deterministic-delete pipeline.
//!
//! Validates two invariants that must hold once the DDP-E series wires the
//! live emitter for `--delete-after` / `--delete-during` transfers:
//!
//! 1. Per-directory `unlink` burst order is identical across repeated runs
//!    of the same input tree (H1, issue #2280).
//! 2. Per-directory `unlink` burst order is invariant under the size of the
//!    rayon worker pool (H2, issue #2281).
//!
//! Cross-directory burst interleaving is permitted to vary; the emitter
//! drains directories in upstream `f_name_cmp` order but worker threads can
//! still publish [`DeletePlan`] entries in any order. The wall-clock unlink
//! sequence within a single directory is the contract we test here.
//!
//! These tests shell out to the built `oc-rsync` binary and capture
//! `unlink`/`unlinkat` calls via `strace`. They are gated behind
//! `OC_RSYNC_DELETE_INTEROP=1` and skip silently when:
//!
//! - `OC_RSYNC_DELETE_INTEROP` is unset (the DDP-E live emitter has not
//!   landed yet, so the determinism guarantee is not in force).
//! - `strace` is not available on `PATH`.
//! - The `oc-rsync` binary cannot be located.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-387`
//!   (`delete_in_dir`, `do_delete_pass`) - emits unlinks per directory in
//!   reverse `f_name_cmp` order.
//! - `target/interop/upstream-src/rsync-3.4.1/delete.c:82-225`
//!   (`delete_item`) - dispatches the actual `unlink`/`rmdir` syscall.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use proptest::prelude::*;
use tempfile::TempDir;

/// Environment variable that opts the test suite in. Until the DDP-E series
/// wires the live emitter, leaving this unset keeps the suite green.
const GATE_ENV: &str = "OC_RSYNC_DELETE_INTEROP";

/// Upper bound for an individual oc-rsync invocation inside a property test.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Number of repeated runs used by the across-runs property (H1).
const RUNS_PER_CASE_H1: usize = 3;

/// Worker pool sizes exercised by the thread-count property (H2).
const THREAD_COUNTS_H2: &[usize] = &[1, 2, 4, 8, 16];

/// Distilled `unlink`/`unlinkat`/`rmdir` event observed via `strace`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnlinkEvent {
    /// Absolute path passed to the syscall.
    path: PathBuf,
    /// True for `rmdir` (directory removal), false for file/symlink unlink.
    is_dir: bool,
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    /// H1: per-directory unlink burst order must be byte-identical across
    /// repeated runs of the same input tree.
    #[test]
    fn delete_sequence_is_deterministic_across_runs(
        seed in any::<u64>(),
        file_count in 1usize..=32,
        dir_count in 1usize..=8,
        mode in delete_mode_strategy(),
    ) {
        let Some(ctx) = TestContext::try_new() else {
            return Ok(());
        };

        let mut groups: Vec<BTreeMap<PathBuf, Vec<UnlinkEvent>>> =
            Vec::with_capacity(RUNS_PER_CASE_H1);
        for _ in 0..RUNS_PER_CASE_H1 {
            let (src, dst) = build_tree(seed, file_count, dir_count);
            let events = ctx.run_oc_rsync(src.path(), dst.path(), mode, None);
            groups.push(group_by_directory(&events));
        }

        let baseline = &groups[0];
        for (run_idx, run) in groups.iter().enumerate().skip(1) {
            prop_assert_eq!(
                run_directory_keys(run),
                run_directory_keys(baseline),
                "run {} touched a different directory set than the baseline",
                run_idx,
            );
            for (dir, events) in baseline {
                prop_assert_eq!(
                    run.get(dir),
                    Some(events),
                    "per-directory unlink order diverged in run {} at {:?}",
                    run_idx,
                    dir,
                );
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    /// H2: per-directory unlink burst order must be invariant under
    /// `RAYON_NUM_THREADS`.
    #[test]
    fn delete_sequence_invariant_to_thread_count(
        seed in any::<u64>(),
        file_count in 1usize..=32,
        dir_count in 1usize..=8,
        mode in delete_mode_strategy(),
    ) {
        let Some(ctx) = TestContext::try_new() else {
            return Ok(());
        };

        let mut groups: Vec<(usize, BTreeMap<PathBuf, Vec<UnlinkEvent>>)> =
            Vec::with_capacity(THREAD_COUNTS_H2.len());
        for &threads in THREAD_COUNTS_H2 {
            let (src, dst) = build_tree(seed, file_count, dir_count);
            let events = ctx.run_oc_rsync(src.path(), dst.path(), mode, Some(threads));
            groups.push((threads, group_by_directory(&events)));
        }

        let (baseline_threads, baseline) = &groups[0];
        for (threads, run) in groups.iter().skip(1) {
            prop_assert_eq!(
                run_directory_keys(run),
                run_directory_keys(baseline),
                "thread count {} touched a different directory set than {}",
                threads,
                baseline_threads,
            );
            for (dir, events) in baseline {
                prop_assert_eq!(
                    run.get(dir),
                    Some(events),
                    "per-directory unlink order diverged at RAYON_NUM_THREADS={} \
                     (baseline {}) at {:?}",
                    threads,
                    baseline_threads,
                    dir,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// The two delete modes that exercise the parallel-deterministic pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteMode {
    During,
    After,
}

impl DeleteMode {
    fn flag(self) -> &'static str {
        match self {
            DeleteMode::During => "--delete-during",
            DeleteMode::After => "--delete-after",
        }
    }
}

fn delete_mode_strategy() -> impl Strategy<Value = DeleteMode> {
    prop_oneof![Just(DeleteMode::During), Just(DeleteMode::After)]
}

// ---------------------------------------------------------------------------
// Test context: locates binaries and decides whether to skip.
// ---------------------------------------------------------------------------

struct TestContext {
    oc_rsync: PathBuf,
    strace: PathBuf,
}

impl TestContext {
    fn try_new() -> Option<Self> {
        if env::var_os(GATE_ENV).is_none() {
            return None;
        }
        let oc_rsync = locate_oc_rsync()?;
        let strace = locate_strace()?;
        Some(Self { oc_rsync, strace })
    }

    fn run_oc_rsync(
        &self,
        src: &Path,
        dst: &Path,
        mode: DeleteMode,
        rayon_threads: Option<usize>,
    ) -> Vec<UnlinkEvent> {
        let strace_log = src
            .parent()
            .unwrap_or_else(|| Path::new("/tmp"))
            .join(format!("strace-{}.log", std::process::id()));

        let mut src_arg = src.as_os_str().to_os_string();
        src_arg.push("/");

        let mut cmd = Command::new(&self.strace);
        cmd.arg("-f")
            .arg("-qq")
            .arg("-e")
            .arg("trace=unlink,unlinkat,rmdir")
            .arg("-o")
            .arg(&strace_log)
            .arg(&self.oc_rsync)
            .arg("-a")
            .arg(mode.flag())
            .arg(src_arg)
            .arg(dst);

        if let Some(threads) = rayon_threads {
            cmd.env("RAYON_NUM_THREADS", threads.to_string());
        }

        run_with_timeout(cmd, RUN_TIMEOUT);

        let log = fs::read_to_string(&strace_log).unwrap_or_default();
        let _ = fs::remove_file(&strace_log);
        parse_strace_log(&log, dst)
    }
}

// ---------------------------------------------------------------------------
// Tree builder
// ---------------------------------------------------------------------------

/// Deterministic source/destination tree builder.
///
/// Returns owned [`TempDir`] handles so the caller controls cleanup. The
/// source contains a subset of names; the destination contains the full set
/// so the receiver has files and directories to delete.
fn build_tree(seed: u64, file_count: usize, dir_count: usize) -> (TempDir, TempDir) {
    let src = TempDir::new().expect("create source tempdir");
    let dst = TempDir::new().expect("create destination tempdir");

    let mut rng = SplitMix64::new(seed);

    // Create directories deterministically.
    let mut dirs: Vec<PathBuf> = Vec::with_capacity(dir_count + 1);
    dirs.push(PathBuf::new());
    for i in 0..dir_count {
        let name = format!("dir_{:02}", i);
        let rel = PathBuf::from(name);
        fs::create_dir_all(src.path().join(&rel)).expect("mkdir src");
        fs::create_dir_all(dst.path().join(&rel)).expect("mkdir dst");
        dirs.push(rel);
    }

    // Sprinkle files. Roughly half live only in the destination so the
    // receiver has something to delete; the rest exist in both trees.
    for i in 0..file_count {
        let dir_idx = (rng.next_u64() as usize) % dirs.len();
        let parent = &dirs[dir_idx];
        let name = format!("file_{:02}.dat", i);
        let rel = parent.join(&name);
        let payload = format!("seed={seed} file={i}\n").into_bytes();

        // Always present in dst (so deletion has work to do half the time).
        fs::write(dst.path().join(&rel), &payload).expect("write dst");

        if rng.next_u64() % 2 == 0 {
            fs::write(src.path().join(&rel), &payload).expect("write src");
        }
    }

    // Add a few destination-only files in each directory so every directory
    // contributes at least one unlink to the burst sequence under check.
    for (idx, parent) in dirs.iter().enumerate() {
        let name = format!("only_dst_{:02}.dat", idx);
        let rel = parent.join(&name);
        let payload = format!("only_dst {idx}\n").into_bytes();
        fs::write(dst.path().join(&rel), &payload).expect("write dst-only");
    }

    (src, dst)
}

// ---------------------------------------------------------------------------
// strace parsing + grouping
// ---------------------------------------------------------------------------

/// Parse `unlink`/`unlinkat`/`rmdir` lines emitted by `strace -f -qq`.
///
/// Only events touching paths under `dst_root` are retained so we ignore
/// `/tmp/...` cleanup that the binary itself might do.
fn parse_strace_log(log: &str, dst_root: &Path) -> Vec<UnlinkEvent> {
    let dst_str = dst_root.to_string_lossy().to_string();
    let mut events = Vec::new();

    for raw in log.lines() {
        // strace -f prefixes each line with a PID; strip it.
        let line = strip_pid_prefix(raw);

        if let Some((path, is_dir)) = parse_syscall_line(line)
            && path.to_string_lossy().starts_with(&dst_str)
        {
            events.push(UnlinkEvent { path, is_dir });
        }
    }

    events
}

fn strip_pid_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx > 0 && idx < bytes.len() && bytes[idx] == b' ' {
        trimmed[idx + 1..].trim_start()
    } else {
        trimmed
    }
}

fn parse_syscall_line(line: &str) -> Option<(PathBuf, bool)> {
    // Forms we accept:
    //   unlink("/path")             = 0
    //   unlinkat(AT_FDCWD, "/path", 0)         = 0
    //   unlinkat(AT_FDCWD, "/path", AT_REMOVEDIR) = 0
    //   rmdir("/path")              = 0
    let (name, rest) = line.split_once('(')?;
    match name {
        "unlink" => {
            let path = extract_quoted(rest)?;
            Some((PathBuf::from(path), false))
        }
        "rmdir" => {
            let path = extract_quoted(rest)?;
            Some((PathBuf::from(path), true))
        }
        "unlinkat" => {
            let path = extract_quoted(rest)?;
            let is_dir = rest.contains("AT_REMOVEDIR");
            Some((PathBuf::from(path), is_dir))
        }
        _ => None,
    }
}

fn extract_quoted(rest: &str) -> Option<String> {
    let start = rest.find('"')? + 1;
    let tail = &rest[start..];
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

/// Group events by parent directory, preserving the wall-clock order
/// within each group.
fn group_by_directory(events: &[UnlinkEvent]) -> BTreeMap<PathBuf, Vec<UnlinkEvent>> {
    let mut grouped: BTreeMap<PathBuf, Vec<UnlinkEvent>> = BTreeMap::new();
    for ev in events {
        let parent = ev
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(PathBuf::new);
        grouped.entry(parent).or_default().push(ev.clone());
    }
    grouped
}

fn run_directory_keys(map: &BTreeMap<PathBuf, Vec<UnlinkEvent>>) -> Vec<&Path> {
    map.keys().map(PathBuf::as_path).collect()
}

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(env_path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(env_path) = env::var_os("OC_RSYNC_BIN") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Some(path);
        }
    }

    // Walk up from the engine crate manifest dir to the workspace target.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cursor = manifest_dir.as_path();
    while let Some(parent) = cursor.parent() {
        for profile in ["debug", "release", "dist"] {
            let candidate = parent.join("target").join(profile).join("oc-rsync");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        cursor = parent;
    }
    None
}

fn locate_strace() -> Option<PathBuf> {
    if let Some(env_path) = env::var_os("OC_RSYNC_STRACE") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Some(path);
        }
    }
    let which = Command::new("sh")
        .arg("-c")
        .arg("command -v strace 2>/dev/null")
        .output()
        .ok()?;
    if !which.status.success() {
        return None;
    }
    let trimmed = String::from_utf8(which.stdout).ok()?.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if path.is_file() { Some(path) } else { None }
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) {
    use std::process::Stdio;
    use std::thread::sleep;
    use std::time::Instant;

    let mut child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return,
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Ok(None) => sleep(Duration::from_millis(50)),
            Err(_) => return,
        }
    }
}

// ---------------------------------------------------------------------------
// Deterministic PRNG
// ---------------------------------------------------------------------------

/// SplitMix64 - tiny deterministic RNG so seeded inputs reproduce exactly
/// across runs. Kept inline to avoid pulling in a `rand` dev-dep.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the helpers (always-on, do not require strace).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn extracts_quoted_path() {
        let path = extract_quoted("\"/tmp/foo/bar\", 0").unwrap();
        assert_eq!(path, "/tmp/foo/bar");
    }

    #[test]
    fn parses_unlink_syscall() {
        let (path, is_dir) = parse_syscall_line("unlink(\"/dst/a\")          = 0").unwrap();
        assert_eq!(path, PathBuf::from("/dst/a"));
        assert!(!is_dir);
    }

    #[test]
    fn parses_unlinkat_file() {
        let (path, is_dir) =
            parse_syscall_line("unlinkat(AT_FDCWD, \"/dst/a\", 0)        = 0").unwrap();
        assert_eq!(path, PathBuf::from("/dst/a"));
        assert!(!is_dir);
    }

    #[test]
    fn parses_unlinkat_directory() {
        let (path, is_dir) =
            parse_syscall_line("unlinkat(AT_FDCWD, \"/dst/d\", AT_REMOVEDIR) = 0").unwrap();
        assert_eq!(path, PathBuf::from("/dst/d"));
        assert!(is_dir);
    }

    #[test]
    fn parses_rmdir_syscall() {
        let (path, is_dir) = parse_syscall_line("rmdir(\"/dst/d\")           = 0").unwrap();
        assert_eq!(path, PathBuf::from("/dst/d"));
        assert!(is_dir);
    }

    #[test]
    fn ignores_unrelated_syscall() {
        assert!(parse_syscall_line("openat(AT_FDCWD, \"/foo\", O_RDONLY) = 3").is_none());
    }

    #[test]
    fn strips_pid_prefix_when_present() {
        let stripped = strip_pid_prefix("12345 unlink(\"/dst/a\")           = 0");
        assert!(stripped.starts_with("unlink("));
    }

    #[test]
    fn leaves_line_unchanged_when_no_pid() {
        let stripped = strip_pid_prefix("unlink(\"/dst/a\")           = 0");
        assert!(stripped.starts_with("unlink("));
    }

    #[test]
    fn group_by_directory_preserves_order() {
        let events = vec![
            UnlinkEvent {
                path: PathBuf::from("/dst/d1/a"),
                is_dir: false,
            },
            UnlinkEvent {
                path: PathBuf::from("/dst/d2/x"),
                is_dir: false,
            },
            UnlinkEvent {
                path: PathBuf::from("/dst/d1/b"),
                is_dir: false,
            },
        ];
        let grouped = group_by_directory(&events);
        let d1 = grouped.get(Path::new("/dst/d1")).expect("d1 group");
        assert_eq!(d1.len(), 2);
        assert_eq!(d1[0].path, PathBuf::from("/dst/d1/a"));
        assert_eq!(d1[1].path, PathBuf::from("/dst/d1/b"));
    }

    #[test]
    fn splitmix_is_deterministic_for_same_seed() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn parse_strace_log_filters_paths_outside_dst() {
        let log = "unlink(\"/dst/a\") = 0\nunlink(\"/other/b\") = 0\n";
        let events = parse_strace_log(log, Path::new("/dst"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, PathBuf::from("/dst/a"));
    }
}
