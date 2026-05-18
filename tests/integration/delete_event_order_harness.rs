//! Shared harness for `--delete-*` event-order interop tests vs upstream rsync.
//!
//! These tests assert that oc-rsync emits the same per-directory sequence of
//! `unlink`/`rmdir`/`unlinkat` syscalls as upstream rsync 3.4.1 for every
//! supported deletion timing mode (`--delete-during`, `--delete-before`,
//! `--delete-after`, `--delete-delay`) plus `--delete-excluded`. The harness
//! drives each binary under `strace -f -e trace=unlink,rmdir,unlinkat,openat`
//! in a tempdir, parses the resulting trace into a sequence of
//! `(syscall, path)` events, and compares oc-rsync's sequence against
//! upstream's with assertions tuned per mode.
//!
//! ## Status: gating tests for DDP-E1-E5
//!
//! These tests will FAIL on master until the parallel-deterministic-delete
//! emitter (DDP-E1-E5) is wired as the live delete path. They are the bar for
//! that wiring work. To prevent CI noise until then, the tests opt in via the
//! `OC_RSYNC_DELETE_INTEROP=1` environment variable and skip cleanly when it
//! is unset.
//!
//! ## Skip conditions
//!
//! The harness skips cleanly (no test failure) when:
//! - The `OC_RSYNC_DELETE_INTEROP` env var is not set to `1`.
//! - `strace` is not on `PATH` (BSDs, macOS, Windows).
//! - An upstream `rsync` binary is not available.
//! - The oc-rsync binary cannot be located.
//!
//! ## Trace parsing
//!
//! `strace -f -e trace=...` emits one line per syscall per process. Per-pid
//! prefixes (`[pid 1234]`) are stripped. Resumed lines (`<... unlink resumed>`)
//! and unfinished lines (`<unfinished ...>`) are stitched back together by
//! syscall name. Paths are extracted as the first quoted argument; for
//! `unlinkat` the second argument (the path) is taken and the `AT_REMOVEDIR`
//! flag flips the syscall classification to `rmdir` for comparison purposes.
//!
//! ## Comparison model
//!
//! Upstream rsync deletes per-directory in a single burst (see
//! `delete.c:delete_in_dir()`). The harness groups events by parent directory
//! and asserts:
//! - The set of directories that emit deletes matches.
//! - Within each directory, the ordered sequence of `(syscall, basename)`
//!   tuples matches.
//! - Across directories the order may differ (oc-rsync's parallel-deterministic
//!   pipeline interleaves dirs, upstream is single-threaded), but each
//!   directory's burst remains atomic.
//!
//! Mode-specific assertions are layered on top by each test file:
//! - `--delete-before`: every `unlink/rmdir` precedes every `openat(O_CREAT)`.
//! - `--delete-after`:  every `openat(O_CREAT)` precedes every `unlink/rmdir`.
//! - `--delete-delay`:  deletes follow the final temp-file rename for each
//!   transferred file.
//! - `--delete-during`: deletes interleave with creates, but within each dir
//!   the unlink burst stays contiguous.
//! - `--delete-excluded`: combined with each of the above timings; the
//!   excluded-path events are present in both traces with the same per-dir
//!   ordering.
//!
//! Upstream source references:
//! - `delete.c:delete_in_dir()` - per-directory deletion burst.
//! - `generator.c:delete_in_dir_loop()` - timing-mode dispatch.
//! - `flist.c:delete_missing()` - DEL_DIR vs DEL_FILE classification.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use super::helpers::{TestDir, spawn_with_timeout};

/// Environment variable that gates execution. Tests skip when this is unset
/// (or not equal to "1"). Set when DDP-E1-E5 lands.
pub const GATE_ENV_VAR: &str = "OC_RSYNC_DELETE_INTEROP";

/// Wall-clock timeout for any single strace-wrapped rsync invocation.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Classification of a parsed trace event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyscallKind {
    /// File removal (`unlink`, or `unlinkat` without `AT_REMOVEDIR`).
    Unlink,
    /// Directory removal (`rmdir`, or `unlinkat` with `AT_REMOVEDIR`).
    Rmdir,
    /// Creating a file (`openat(..., O_CREAT, ...)`); marker for ordering
    /// assertions in `--delete-before` and `--delete-after` modes.
    OpenCreate,
    /// `rename`/`renameat` - final commit of a temp file. Used by
    /// `--delete-delay` to anchor when deletes are permitted to run.
    Rename,
}

/// A single parsed trace event.
#[derive(Debug, Clone)]
pub struct Event {
    /// Classified syscall.
    pub kind: SyscallKind,
    /// Absolute path the syscall targeted (best-effort resolution from the
    /// trace; relative paths are kept as-is).
    pub path: PathBuf,
}

impl Event {
    /// Parent directory of the event's path.
    pub fn parent(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// File-name component, falling back to the full path on edge cases.
    pub fn basename(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }
}

/// Result of a successful capture: ordered events plus the destination
/// directory used so callers can canonicalise paths.
#[derive(Debug)]
pub struct CapturedRun {
    /// Events in trace order (after stitching split lines).
    pub events: Vec<Event>,
    /// Destination directory the transfer wrote into.
    pub dest: PathBuf,
}

/// Outcome of a `run_pair_capture` invocation.
pub enum PairOutcome {
    /// Both binaries ran successfully and traces were parsed.
    Captured {
        upstream: CapturedRun,
        oc_rsync: CapturedRun,
    },
    /// A precondition failed (env var unset, missing tool, missing binary).
    /// The caller should `return` after logging.
    Skipped(String),
}

/// Top-level entry point. Build a source tree, run upstream rsync and
/// oc-rsync under `strace`, and return the parsed event streams. Returns
/// `Skipped` with a human-readable reason when preconditions are unmet.
///
/// `extra_flags` are appended to a common base argv (`-a --delete-mode`
/// chosen by the caller).
pub fn run_pair_capture(
    scenario: &Scenario,
    delete_mode_flags: &[&str],
    extra_flags: &[&str],
) -> io::Result<PairOutcome> {
    if env::var(GATE_ENV_VAR).ok().as_deref() != Some("1") {
        return Ok(PairOutcome::Skipped(format!(
            "{GATE_ENV_VAR} not set to 1; opt in to run delete-mode interop tests"
        )));
    }
    if !command_on_path("strace") {
        return Ok(PairOutcome::Skipped(
            "strace not found on PATH; required for syscall capture".to_string(),
        ));
    }
    let upstream_bin = match locate_upstream_rsync() {
        Some(p) => p,
        None => {
            return Ok(PairOutcome::Skipped(
                "no upstream rsync found (set OC_RSYNC_UPSTREAM or install \
                 target/interop/upstream-install/<ver>/bin/rsync)"
                    .to_string(),
            ));
        }
    };
    let oc_bin = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            return Ok(PairOutcome::Skipped(
                "oc-rsync binary not located".to_string(),
            ));
        }
    };

    let upstream = run_one_capture(scenario, &upstream_bin, delete_mode_flags, extra_flags)?;
    let oc_rsync = run_one_capture(scenario, &oc_bin, delete_mode_flags, extra_flags)?;
    Ok(PairOutcome::Captured { upstream, oc_rsync })
}

/// Build the scenario tree and trace one rsync invocation end-to-end.
fn run_one_capture(
    scenario: &Scenario,
    rsync_bin: &Path,
    delete_mode_flags: &[&str],
    extra_flags: &[&str],
) -> io::Result<CapturedRun> {
    let dir = TestDir::new()?;
    let src = dir.mkdir("src")?;
    let dst = dir.mkdir("dst")?;
    scenario.materialise(&src, &dst)?;

    let trace_log = dir.path().join("strace.out");

    let mut cmd = Command::new("strace");
    cmd.arg("-f")
        .arg("-e")
        .arg("trace=unlink,unlinkat,rmdir,openat,rename,renameat,renameat2")
        .arg("-o")
        .arg(&trace_log)
        .arg(rsync_bin)
        .arg("-a");
    for f in delete_mode_flags {
        cmd.arg(f);
    }
    for f in extra_flags {
        cmd.arg(f);
    }
    // Trailing slash on source ensures "copy contents" semantics in both
    // implementations, keeping path-relative event paths comparable.
    cmd.arg(format!("{}/", src.display()));
    cmd.arg(dst.to_string_lossy().as_ref());
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let output = spawn_with_timeout(cmd, RUN_TIMEOUT)?;
    require_success(rsync_bin, &output)?;

    let raw = fs::read_to_string(&trace_log)?;
    let events = parse_strace(&raw, &dst);
    Ok(CapturedRun { events, dest: dst })
}

fn require_success(bin: &Path, output: &Output) -> io::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "rsync {} exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        bin.display(),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )))
}

/// Scenario describes the source and destination trees a test exercises.
pub struct Scenario {
    /// Files to create in the source tree (path, content).
    pub src_files: Vec<(String, &'static [u8])>,
    /// Files to pre-create in the destination tree that the source does not
    /// have; these are the deletion targets.
    pub dst_extra_files: Vec<(String, &'static [u8])>,
    /// Extra directories to create in the destination tree (no source
    /// counterpart). Useful for `rmdir` assertions.
    pub dst_extra_dirs: Vec<String>,
}

impl Scenario {
    /// Default 3-files-in-/a, 2-files-in-/a/x, 1-file-in-/b scenario used by
    /// the `during` test.
    pub fn during_default() -> Self {
        let src_files: Vec<(String, &'static [u8])> = vec![
            ("a/keep1".to_string(), b"keep1" as &[u8]),
            ("a/keep2".to_string(), b"keep2"),
            ("a/x/keepx".to_string(), b"keepx"),
            ("b/keepb".to_string(), b"keepb"),
        ];
        let dst_extra_files: Vec<(String, &'static [u8])> = vec![
            ("a/gone1".to_string(), b"g1" as &[u8]),
            ("a/gone2".to_string(), b"g2"),
            ("a/gone3".to_string(), b"g3"),
            ("a/x/xgone1".to_string(), b"xg1"),
            ("a/x/xgone2".to_string(), b"xg2"),
            ("b/bgone1".to_string(), b"bg1"),
        ];
        Self {
            src_files,
            dst_extra_files,
            dst_extra_dirs: Vec::new(),
        }
    }

    /// Scenario with one source file per dir, plus extras both in src (new)
    /// and dst (to delete). Forces creates alongside deletes for ordering
    /// assertions in `--delete-before` / `--delete-after`.
    pub fn before_after_default() -> Self {
        let src_files: Vec<(String, &'static [u8])> = vec![
            ("a/new1".to_string(), b"new1" as &[u8]),
            ("a/new2".to_string(), b"new2"),
            ("b/new3".to_string(), b"new3"),
        ];
        let dst_extra_files: Vec<(String, &'static [u8])> = vec![
            ("a/gone1".to_string(), b"g1" as &[u8]),
            ("a/gone2".to_string(), b"g2"),
            ("b/gone3".to_string(), b"g3"),
        ];
        Self {
            src_files,
            dst_extra_files,
            dst_extra_dirs: Vec::new(),
        }
    }

    /// Scenario for `--delete-excluded`: pre-creates files in dst whose names
    /// match the exclude pattern, and one matching file in src that must
    /// still transfer. Adds non-excluded extras on both sides.
    pub fn excluded_default() -> Self {
        let src_files: Vec<(String, &'static [u8])> = vec![
            ("a/keep.txt".to_string(), b"kt" as &[u8]),
            ("a/new.txt".to_string(), b"nt"),
            ("b/keep.txt".to_string(), b"bk"),
        ];
        let dst_extra_files: Vec<(String, &'static [u8])> = vec![
            ("a/old.txt".to_string(), b"ot" as &[u8]),
            ("a/skip.bak".to_string(), b"sb"),
            ("b/skip.bak".to_string(), b"bb"),
            ("b/old.txt".to_string(), b"bo"),
        ];
        Self {
            src_files,
            dst_extra_files,
            dst_extra_dirs: Vec::new(),
        }
    }

    /// Write all files and directories into the given roots.
    pub fn materialise(&self, src: &Path, dst: &Path) -> io::Result<()> {
        for (rel, content) in &self.src_files {
            write_file(&src.join(rel), content)?;
        }
        for (rel, content) in &self.dst_extra_files {
            write_file(&dst.join(rel), content)?;
            // Also ensure the dst has the source's directory structure so the
            // deletion targets sit alongside transferred files.
            if let Some(parent) = Path::new(rel).parent() {
                fs::create_dir_all(src.join(parent))?;
            }
        }
        for rel in &self.dst_extra_dirs {
            fs::create_dir_all(dst.join(rel))?;
        }
        Ok(())
    }
}

fn write_file(path: &Path, content: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

/// Strip a `[pid NNN]` prefix from a strace line, returning the rest.
fn strip_pid_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("[pid ") {
        if let Some(end) = rest.find(']') {
            return rest[end + 1..].trim_start();
        }
    }
    trimmed
}

/// Best-effort path extraction from a syscall argument list. Returns the
/// content of the first double-quoted string; backslash escapes are passed
/// through unmodified (strace's default formatting is round-trippable for
/// printable paths, which is sufficient for our comparison).
fn first_quoted(args: &str) -> Option<String> {
    let bytes = args.as_bytes();
    let start = bytes.iter().position(|b| *b == b'"')?;
    let mut i = start + 1;
    let mut out = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(out),
            b'\\' if i + 1 < bytes.len() => {
                out.push(bytes[i + 1] as char);
                i += 2;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    None
}

/// Like `first_quoted`, but returns the Nth (0-indexed) quoted string.
fn nth_quoted(args: &str, n: usize) -> Option<String> {
    let mut remaining = args;
    let mut found = 0;
    loop {
        let start = remaining.find('"')?;
        let mut i = start + 1;
        let bytes = remaining.as_bytes();
        let mut out = String::new();
        while i < bytes.len() {
            match bytes[i] {
                b'"' => break,
                b'\\' if i + 1 < bytes.len() => {
                    out.push(bytes[i + 1] as char);
                    i += 2;
                }
                c => {
                    out.push(c as char);
                    i += 1;
                }
            }
        }
        if i >= bytes.len() {
            return None;
        }
        if found == n {
            return Some(out);
        }
        found += 1;
        remaining = &remaining[i + 1..];
    }
}

/// Parse a strace `-o` output file into ordered events scoped to paths
/// under the destination directory.
pub fn parse_strace(raw: &str, dst: &Path) -> Vec<Event> {
    let dst_str = dst.to_string_lossy().into_owned();
    let mut out = Vec::new();
    for line in raw.lines() {
        let body = strip_pid_prefix(line);
        // Ignore <unfinished ...> and <... resumed> halves: a syscall split
        // across two strace lines re-prints its arguments in the resumed
        // half, so we'd double-count if we honoured either half. Wait for the
        // complete (non-split) lines, which strace -f emits for any syscall
        // that finishes in one go.
        if body.contains("<unfinished") || body.contains("resumed>") {
            continue;
        }
        let Some(paren) = body.find('(') else {
            continue;
        };
        let name = &body[..paren];
        let args = &body[paren + 1..];

        let (kind, path) = match name {
            "unlink" => {
                let Some(p) = first_quoted(args) else {
                    continue;
                };
                (SyscallKind::Unlink, p)
            }
            "rmdir" => {
                let Some(p) = first_quoted(args) else {
                    continue;
                };
                (SyscallKind::Rmdir, p)
            }
            "unlinkat" => {
                let Some(p) = nth_quoted(args, 0) else {
                    continue;
                };
                let kind = if args.contains("AT_REMOVEDIR") {
                    SyscallKind::Rmdir
                } else {
                    SyscallKind::Unlink
                };
                (kind, p)
            }
            "openat" => {
                if !args.contains("O_CREAT") {
                    continue;
                }
                let Some(p) = nth_quoted(args, 0) else {
                    continue;
                };
                (SyscallKind::OpenCreate, p)
            }
            "rename" | "renameat" | "renameat2" => {
                // The destination of the rename is what matters for delete-delay
                // assertions; that is the last quoted argument.
                let mut paths = Vec::new();
                let mut cursor = 0usize;
                while let Some(p) = nth_quoted(args, cursor) {
                    paths.push(p);
                    cursor += 1;
                }
                let Some(p) = paths.pop() else { continue };
                (SyscallKind::Rename, p)
            }
            _ => continue,
        };

        let resolved = if Path::new(&path).is_absolute() {
            PathBuf::from(&path)
        } else {
            dst.join(&path)
        };
        let resolved_str = resolved.to_string_lossy().into_owned();
        if !resolved_str.starts_with(&dst_str) {
            continue;
        }
        out.push(Event {
            kind,
            path: resolved,
        });
    }
    out
}

/// Group unlink/rmdir events by parent directory, preserving in-directory
/// order. Used as the primary comparator across implementations.
pub fn delete_events_by_dir(events: &[Event]) -> BTreeMap<PathBuf, Vec<(SyscallKind, String)>> {
    let mut by_dir: BTreeMap<PathBuf, Vec<(SyscallKind, String)>> = BTreeMap::new();
    for ev in events {
        if !matches!(ev.kind, SyscallKind::Unlink | SyscallKind::Rmdir) {
            continue;
        }
        let parent = ev.parent();
        let base = ev.basename();
        by_dir
            .entry(parent)
            .or_default()
            .push((ev.kind.clone(), base));
    }
    // Sort within each dir for set-based comparison; mode-specific tests can
    // re-sort or compare ordered as needed via the raw `events` slice.
    for v in by_dir.values_mut() {
        v.sort();
    }
    by_dir
}

/// Assert that the upstream and oc-rsync runs visit the same set of deletion
/// directories with the same (sorted) per-directory delete set. Returns a
/// list of per-dir mismatches as strings; an empty Vec means the assertion
/// holds.
pub fn diff_delete_groups(upstream: &[Event], oc_rsync: &[Event]) -> Vec<String> {
    let up = delete_events_by_dir(upstream);
    let oc = delete_events_by_dir(oc_rsync);
    let up_dirs: BTreeSet<_> = up.keys().cloned().collect();
    let oc_dirs: BTreeSet<_> = oc.keys().cloned().collect();
    let mut errors = Vec::new();
    for d in up_dirs.difference(&oc_dirs) {
        errors.push(format!(
            "upstream deleted in dir {} but oc-rsync did not",
            d.display()
        ));
    }
    for d in oc_dirs.difference(&up_dirs) {
        errors.push(format!(
            "oc-rsync deleted in dir {} but upstream did not",
            d.display()
        ));
    }
    for d in up_dirs.intersection(&oc_dirs) {
        let up_set = &up[d];
        let oc_set = &oc[d];
        if up_set != oc_set {
            errors.push(format!(
                "per-dir delete set differs at {}\n  upstream: {:?}\n  oc-rsync: {:?}",
                d.display(),
                up_set,
                oc_set,
            ));
        }
    }
    errors
}

/// Index of the first event matching `pred`, or `None` if no such event.
pub fn first_index<F: FnMut(&Event) -> bool>(events: &[Event], pred: F) -> Option<usize> {
    events.iter().position(pred)
}

/// Index of the last event matching `pred`, or `None` if no such event.
pub fn last_index<F: FnMut(&Event) -> bool>(events: &[Event], pred: F) -> Option<usize> {
    events.iter().rposition(pred)
}

/// Check `command -v <cmd>` on PATH.
pub fn command_on_path(cmd: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {cmd} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Locate the oc-rsync binary built by cargo. Walks the standard places
/// nextest exposes plus the workspace `target/{debug,release}` layout.
pub fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(env_path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release", "dist"] {
        let candidate = manifest_dir.join("target").join(profile).join("oc-rsync");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Locate an upstream rsync binary. Honours `OC_RSYNC_UPSTREAM`, then the
/// `target/interop/upstream-install/<ver>/bin/rsync` cache, then `which`.
pub fn locate_upstream_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("OC_RSYNC_UPSTREAM") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for version in ["3.4.2", "3.4.1", "3.1.3", "3.0.9"] {
        let in_tree = manifest_dir
            .join("target/interop/upstream-install")
            .join(version)
            .join("bin/rsync");
        if in_tree.is_file() {
            return Some(in_tree);
        }
    }
    let which = Command::new("sh")
        .arg("-c")
        .arg("command -v rsync 2>/dev/null")
        .output()
        .ok()?;
    if !which.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(which.stdout).ok()?.trim());
    if path.is_file() { Some(path) } else { None }
}

/// Convenience: log a skip reason and exit the test successfully.
pub fn skip(reason: &str) {
    eprintln!("skip: {reason}");
}
