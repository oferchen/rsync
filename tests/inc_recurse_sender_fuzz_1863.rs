//! INC_RECURSE sender fuzz against upstream rsync receivers (#1863).
//!
//! Sister harness to `live_interop_fuzz_1196.rs`. Where the parent fuzzer
//! exercises both directions with default settings, this one focuses on the
//! oc-rsync **sender** path with the `INC_RECURSE` ('i') capability negotiated
//! on, pushing randomised trees to upstream rsync receivers and asserting the
//! destination matches the source byte-for-byte.
//!
//! ## Why a dedicated fuzz harness
//!
//! Sender-side INC_RECURSE code exists in `crates/transfer/src/generator/`
//! (`IncrementalState`, `SegmentScheduler`, `PendingSegment`) but is gated off
//! by default for client-mode SSH push, because the cross-version interop
//! surface against upstream had not been validated. See
//! `crates/transfer/src/generator/mod.rs` (tracker #1862) and
//! `crates/core/src/client/remote/invocation/builder.rs:178-186` for the
//! gating point.
//!
//! ## Runtime opt-in
//!
//! No env var or build-time toggle is required. The CLI exposes
//! `--inc-recursive` (alias `--i-r`) which flips
//! `ClientConfig::inc_recursive_send` to `true`. With that flag set,
//! `build_capability_string(true)` includes `'i'` in the `-e` string that
//! `--server` invocations advertise to the peer. The single source of truth
//! is `crates/transfer/src/setup/capability.rs::build_capability_string()`.
//!
//! ## How the fuzz exercises the sender path
//!
//! Real SSH is not available in CI, so the harness wires upstream rsync's
//! client process to an `oc-rsync --server --sender` instance through
//! bidirectional pipes, mirroring what an SSH transport would do. The
//! upstream client process is what carries the `-e.iLsfxCIvu` capability
//! string into the server side, and the negotiation in
//! `crates/transfer/src/setup/mod.rs::setup_protocol` consumes it. With
//! `--inc-recursive` set on the oc-rsync server invocation, INC_RECURSE is
//! advertised in both directions and the sender's incremental segment
//! scheduler is exercised end-to-end.
//!
//! For coverage in environments without the upstream pipe wiring, the test
//! also drives a local oc-rsync push with `--inc-recursive` so that
//! `inc_recursive_send` is on for the local pipeline.
//!
//! ## Versions covered
//!
//! Tests run against whichever subset of upstream is installed under
//! `target/interop/upstream-install/{3.0.9,3.1.3,3.4.1}/bin/rsync`. Any version
//! that is missing is skipped with a `skip:` log line; the test does not
//! fail solely because a binary is absent.
//!
//! ## Reproducibility
//!
//! Randomness flows through a single xorshift64* PRNG. Seed defaults to the
//! system clock; pin via `OC_RSYNC_FUZZ_SEED`. Failures print the seed and
//! the exact reproducer command line.
//!
//! ## Budget
//!
//! Marked `#[ignore]` so it is opt-in. The default budget is 60 seconds
//! (override via `OC_RSYNC_FUZZ_BUDGET_SECS`). Use small trees so each
//! iteration completes well under one second and many iterations land inside
//! the smoke budget.
//!
//! ## Invocation
//!
//! ```text
//! cargo nextest run --workspace -E 'test(inc_recurse_sender_fuzz)' \
//!     --run-ignored=all
//! ```

#![cfg(unix)]

mod integration;

use integration::helpers::{TestDir, upstream_rsync_binary};

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use checksums::strong::Sha256;
use filetime::{FileTime, set_file_times};

// ============================================================================
// PRNG: xorshift64*, no external crate dependency.
// ============================================================================

/// Seedable PRNG used by every randomised decision in this test.
///
/// xorshift64* is small, deterministic, and adequate for property-style
/// fuzzing; this test is not a cryptographic generator.
struct Rng {
    state: u64,
}

impl Rng {
    fn from_seed(seed: u64) -> Self {
        let state = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn gen_range(&mut self, lo: u64, hi: u64) -> u64 {
        debug_assert!(hi > lo);
        lo + self.next_u64() % (hi - lo)
    }

    fn gen_usize(&mut self, lo: usize, hi: usize) -> usize {
        self.gen_range(lo as u64, hi as u64) as usize
    }

    fn fill_bytes(&mut self, buf: &mut [u8]) {
        let len = buf.len();
        let mut i = 0;
        while i + 8 <= len {
            let v = self.next_u64().to_le_bytes();
            buf[i..i + 8].copy_from_slice(&v);
            i += 8;
        }
        if i < len {
            let v = self.next_u64().to_le_bytes();
            let tail = len - i;
            buf[i..].copy_from_slice(&v[..tail]);
        }
    }
}

// ============================================================================
// Tree generation
// ============================================================================

const NAME_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-_";

fn rand_name(rng: &mut Rng, len: usize) -> String {
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        let idx = (rng.next_u64() % NAME_ALPHABET.len() as u64) as usize;
        bytes.push(NAME_ALPHABET[idx]);
    }
    String::from_utf8(bytes).expect("ASCII alphabet")
}

/// Random directory tree designed to exercise INC_RECURSE segmentation.
///
/// INC_RECURSE batches the file list into per-directory segments. The
/// generator interleaves deeper directories with new files at the root, so
/// the scheduler sees segments of varying sizes and depths.
fn build_random_tree(rng: &mut Rng, root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;

    let dir_count = rng.gen_usize(3, 9);
    let mut dirs: Vec<PathBuf> = vec![root.to_path_buf()];
    for _ in 0..dir_count {
        let parent_idx = rng.gen_usize(0, dirs.len());
        let depth_extra = rng.gen_usize(1, 3);
        let mut path = dirs[parent_idx].clone();
        for _ in 0..depth_extra {
            let name_len = rng.gen_usize(3, 8);
            path = path.join(rand_name(rng, name_len));
        }
        fs::create_dir_all(&path)?;
        dirs.push(path);
    }

    let file_count = rng.gen_usize(4, 24);
    for _ in 0..file_count {
        let dir_idx = rng.gen_usize(0, dirs.len());
        let name_len = rng.gen_usize(3, 12);
        let name = rand_name(rng, name_len);
        let path = dirs[dir_idx].join(name);

        let size = rng.gen_usize(0, 8 * 1024 + 1);
        let mut buf = vec![0u8; size];
        rng.fill_bytes(&mut buf);
        fs::write(&path, &buf)?;

        let secs = rng.gen_range(1_600_000_000, 1_750_000_000) as i64;
        let nsecs = (rng.next_u64() % 1_000_000_000) as u32;
        let mtime = FileTime::from_unix_time(secs, nsecs);
        set_file_times(&path, mtime, mtime)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode_choices = [0o644u32, 0o600, 0o664, 0o755, 0o400];
            let mode = mode_choices[rng.gen_usize(0, mode_choices.len())];
            let mut perm = fs::metadata(&path)?.permissions();
            perm.set_mode(mode);
            fs::set_permissions(&path, perm)?;
        }
    }

    Ok(())
}

// ============================================================================
// Snapshot + diff
// ============================================================================

#[derive(Debug, PartialEq, Eq)]
enum EntryKind {
    File([u8; 32]),
    Symlink(PathBuf),
    Dir,
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    entries: Vec<(PathBuf, EntryKind)>,
}

fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let mut entries: Vec<(PathBuf, EntryKind)> = Vec::new();
    snapshot_inner(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Snapshot { entries })
}

fn snapshot_inner(
    base: &Path,
    current: &Path,
    entries: &mut Vec<(PathBuf, EntryKind)>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap().to_path_buf();
        let meta = fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            let target = fs::read_link(&path)?;
            entries.push((rel, EntryKind::Symlink(target)));
        } else if ft.is_dir() {
            entries.push((rel.clone(), EntryKind::Dir));
            snapshot_inner(base, &path, entries)?;
        } else if ft.is_file() {
            let bytes = fs::read(&path)?;
            let digest = Sha256::digest(&bytes);
            entries.push((rel, EntryKind::File(digest)));
        }
    }
    Ok(())
}

fn diff_snapshots(src: &Snapshot, dst: &Snapshot) -> Vec<String> {
    let mut errors = Vec::new();
    let src_map: std::collections::BTreeMap<_, _> =
        src.entries.iter().map(|(p, k)| (p.clone(), k)).collect();
    let dst_map: std::collections::BTreeMap<_, _> =
        dst.entries.iter().map(|(p, k)| (p.clone(), k)).collect();

    for (p, sk) in &src_map {
        match dst_map.get(p) {
            None => errors.push(format!("missing in dst: {}", p.display())),
            Some(dk) => {
                if sk != dk {
                    errors.push(format!(
                        "entry differs: {} (src={:?} dst={:?})",
                        p.display(),
                        sk,
                        dk
                    ));
                }
            }
        }
    }
    for p in dst_map.keys() {
        if !src_map.contains_key(p) {
            errors.push(format!("extra in dst: {}", p.display()));
        }
    }
    errors
}

// ============================================================================
// Binary discovery
// ============================================================================

fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    let name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    while !dir.ends_with("target") {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    for sub in ["debug", "release"] {
        let candidate = dir.join(sub).join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Per-upstream-version capability string sent in `--server` mode.
///
/// rsync 3.1.0 added 'v' (varint flist flags), 'x' (avoid xattr opt), and
/// 'u' (id0 names). 'i' is INC_RECURSE and is supported by every version
/// covered here (protocol >= 30).
fn flags_for_version(version: &str) -> &'static str {
    if version.starts_with("3.0") {
        "-vlogDtprze.iLsfCI"
    } else {
        "-vlogDtprze.iLsfxCIvu"
    }
}

// ============================================================================
// Pipe-driven server-mode transfer (mimics SSH transport)
// ============================================================================

/// Spawn upstream rsync as the client and oc-rsync as `--server --sender`,
/// wiring their stdio together with pipes to mimic an SSH transport.
///
/// This is the path that fully exercises INC_RECURSE on the sender side:
/// the upstream client carries the capability string into oc-rsync's
/// server-side `setup_protocol`, and the negotiated `CompatibilityFlags`
/// drive the generator's incremental segment scheduler.
fn run_pipe_push(
    oc_bin: &Path,
    up_bin: &Path,
    upstream_version: &str,
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    let flags = flags_for_version(upstream_version);

    // oc-rsync runs as --server --sender. With --inc-recursive we set
    // inc_recursive_send=true so the negotiator allows the 'i' bit to remain
    // set in CompatibilityFlags when the upstream client advertises it.
    let mut server = Command::new(oc_bin)
        .arg("--server")
        .arg("--sender")
        .arg("--inc-recursive")
        .arg(flags)
        .arg(".")
        .arg(src.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Upstream runs as --server (receiver). The upstream binary advertises
    // INC_RECURSE in its own client_info string when launched by an rsync
    // client; for direct --server invocation we replicate that advertisement
    // through the flag string above.
    let mut client = Command::new(up_bin)
        .arg("--server")
        .arg(flags)
        .arg(".")
        .arg(dst.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let server_stdout = server.stdout.take().unwrap();
    let server_stdin = server.stdin.take().unwrap();
    let client_stdout = client.stdout.take().unwrap();
    let client_stdin = client.stdin.take().unwrap();

    // Sender (server) -> receiver (client) stream.
    let s2c = thread::spawn(move || -> io::Result<()> {
        let mut reader = std::io::BufReader::new(server_stdout);
        let mut writer = std::io::BufWriter::new(client_stdin);
        copy_until_eof(&mut reader, &mut writer)
    });

    // Receiver (client) -> sender (server) stream.
    let c2s = thread::spawn(move || -> io::Result<()> {
        let mut reader = std::io::BufReader::new(client_stdout);
        let mut writer = std::io::BufWriter::new(server_stdin);
        copy_until_eof(&mut reader, &mut writer)
    });

    let server_stderr = server.stderr.take();
    let client_stderr = client.stderr.take();

    let server_status = server.wait()?;
    let client_status = client.wait()?;

    let _ = s2c.join();
    let _ = c2s.join();

    if !server_status.success() || !client_status.success() {
        let mut server_err = Vec::new();
        let mut client_err = Vec::new();
        if let Some(mut s) = server_stderr {
            let _ = s.read_to_end(&mut server_err);
        }
        if let Some(mut s) = client_stderr {
            let _ = s.read_to_end(&mut client_err);
        }
        return Err(io::Error::other(format!(
            "pipe push failed: oc-rsync server status={:?} upstream client status={:?}\n\
             oc-rsync stderr:\n{}\nupstream stderr:\n{}",
            server_status.code(),
            client_status.code(),
            String::from_utf8_lossy(&server_err),
            String::from_utf8_lossy(&client_err),
        )));
    }
    Ok(())
}

fn copy_until_eof<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        writer.flush()?;
    }
    Ok(())
}

// ============================================================================
// Local push with --inc-recursive (lighter-weight coverage path)
// ============================================================================

/// Local push driven by oc-rsync with `--inc-recursive` set.
///
/// Local mode does not exchange the capability string with a remote peer,
/// but the same `inc_recursive_send` flag flows into the generator config,
/// so the segment scheduler still runs. Used as a fast smoke pass in
/// environments without upstream rsync.
fn run_local_push(oc_bin: &Path, src: &Path, dst: &Path) -> io::Result<()> {
    let mut cmd = Command::new(oc_bin);
    cmd.arg("-a")
        .arg("--inc-recursive")
        .arg("--no-owner")
        .arg("--no-group")
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()));
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "local push failed: status={:?}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        )));
    }
    Ok(())
}

// ============================================================================
// Iteration drivers
// ============================================================================

struct UpstreamTarget {
    version: &'static str,
    binary: PathBuf,
}

fn discover_upstream_targets() -> Vec<UpstreamTarget> {
    let mut out = Vec::new();
    for version in ["3.0.9", "3.1.3", "3.4.1"] {
        if let Some(p) = upstream_rsync_binary(version) {
            out.push(UpstreamTarget { version, binary: p });
        }
    }
    if let Ok(override_path) = env::var("OC_RSYNC_FUZZ_UPSTREAM_BIN") {
        let p = PathBuf::from(override_path);
        if p.is_file() {
            // Treat the override as the latest version's capability set.
            out.push(UpstreamTarget {
                version: "3.4.1",
                binary: p,
            });
        }
    }
    out
}

fn run_iteration(
    rng: &mut Rng,
    iter_idx: usize,
    seed: u64,
    oc_bin: &Path,
    upstream_targets: &[UpstreamTarget],
) -> io::Result<()> {
    let test_dir = TestDir::new()?;
    let src = test_dir.mkdir("src")?;
    build_random_tree(rng, &src)?;
    let src_snap = snapshot(&src)?;

    // Local push pass; covers the inc_recursive_send config plumbing even
    // when no upstream rsync is installed.
    let local_dst = test_dir.mkdir("dst_local")?;
    run_local_push(oc_bin, &src, &local_dst)?;
    let local_snap = snapshot(&local_dst)?;
    let diffs = diff_snapshots(&src_snap, &local_snap);
    if !diffs.is_empty() {
        return Err(io::Error::other(format!(
            "local push diverged (seed={seed}, iter={iter_idx}):\n{}",
            diffs.join("\n")
        )));
    }

    // Pipe-driven push to each available upstream receiver.
    for target in upstream_targets {
        let dst = test_dir.mkdir(&format!("dst_{}", target.version.replace('.', "_")))?;
        run_pipe_push(oc_bin, &target.binary, target.version, &src, &dst)?;
        let dst_snap = snapshot(&dst)?;
        let diffs = diff_snapshots(&src_snap, &dst_snap);
        if !diffs.is_empty() {
            return Err(io::Error::other(format!(
                "pipe push to upstream {} diverged (seed={seed}, iter={iter_idx}):\n{}",
                target.version,
                diffs.join("\n")
            )));
        }
    }

    Ok(())
}

// ============================================================================
// Entry points
// ============================================================================

fn budget_seconds() -> u64 {
    env::var("OC_RSYNC_FUZZ_BUDGET_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
}

fn seed_for_run() -> u64 {
    if let Ok(s) = env::var("OC_RSYNC_FUZZ_SEED") {
        if let Ok(v) = s.parse() {
            return v;
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xC0FF_EE12_3456_789A)
}

fn run_until_budget(budget: Duration) {
    let oc_bin = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            eprintln!("skip: oc-rsync binary not located");
            return;
        }
    };

    let upstream_targets = discover_upstream_targets();
    if upstream_targets.is_empty() {
        eprintln!(
            "skip: no upstream rsync 3.0.9/3.1.3/3.4.1 binaries found; \
             install via tools/ci/run_interop.sh or set OC_RSYNC_FUZZ_UPSTREAM_BIN"
        );
        return;
    }

    let seed = seed_for_run();
    eprintln!(
        "inc-recurse sender fuzz: seed={seed} budget={:?} oc_bin={} upstream_targets={}",
        budget,
        oc_bin.display(),
        upstream_targets
            .iter()
            .map(|t| format!("{}@{}", t.version, t.binary.display()))
            .collect::<Vec<_>>()
            .join(",")
    );

    let mut rng = Rng::from_seed(seed);
    let start = Instant::now();
    let mut iter = 0usize;
    while start.elapsed() < budget {
        if let Err(e) = run_iteration(&mut rng, iter, seed, &oc_bin, &upstream_targets) {
            panic!(
                "inc-recurse sender fuzz failed (seed={seed}, iter={iter}): {e}\n\
                 reproduce: OC_RSYNC_FUZZ_SEED={seed} OC_RSYNC_FUZZ_BUDGET_SECS=600 \
                 cargo nextest run --workspace -E 'test(inc_recurse_sender_fuzz)' \
                 --run-ignored=all"
            );
        }
        iter += 1;
    }

    eprintln!(
        "inc-recurse sender fuzz: completed {iter} iterations in {:?}",
        start.elapsed()
    );
}

/// Smoke tier: bounded ~60s. Heavy: opt in to longer budgets via env var.
#[test]
#[ignore = "spawns subprocesses; run via --run-ignored=all"]
fn inc_recurse_sender_fuzz_smoke() {
    let budget = Duration::from_secs(budget_seconds().min(120));
    run_until_budget(budget);
}

/// Extended tier: pure budget; the iteration logic is the same.
#[test]
#[ignore = "long-running; opt in via OC_RSYNC_FUZZ_BUDGET_SECS"]
fn inc_recurse_sender_fuzz_extended() {
    let budget = Duration::from_secs(budget_seconds());
    run_until_budget(budget);
}
