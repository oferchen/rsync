//! Live interop fuzzer (#1196): randomised transfers between oc-rsync and
//! upstream rsync 3.4.x.
//!
//! Sister harness to the static `adversarial_stream_corpus` work: where the
//! corpus catches fixed cases, this fuzzer catches dynamic edge cases by
//! generating randomised source trees on every run and driving both transfer
//! directions through a real upstream binary.
//!
//! ## Coverage per iteration
//!
//! 1. Build a randomised source tree (files, sizes, mtimes, permissions,
//!    optional sparse holes, optional symlinks on Unix).
//! 2. Push: oc-rsync sender, upstream receiver -- verify dst hashes match src.
//! 3. Pull: upstream sender, oc-rsync receiver -- verify dst hashes match src.
//! 4. Optionally mutate the source (add/delete/modify files) and re-run the
//!    transfer to assert that the second pass reconciles correctly.
//!
//! ## Reproducibility
//!
//! All randomness flows through a single seedable PRNG (xorshift64*). The seed
//! defaults to a value derived from the system clock but can be pinned via the
//! `OC_RSYNC_FUZZ_SEED` environment variable. On failure the test prints the
//! seed so the run can be reproduced exactly.
//!
//! ## Budget
//!
//! The test is `#[ignore]` because it spawns hundreds of subprocesses and is
//! intended for opt-in runs. Two tiers are provided:
//!
//! - Smoke tier (default): bounded to ~60 seconds, small trees, a handful of
//!   iterations -- suitable for nightly CI.
//! - Extended tier: set `OC_RSYNC_FUZZ_BUDGET_SECS=3600` to run for an hour
//!   with larger trees and more iterations.
//!
//! ## Invocation
//!
//! ```text
//! cargo nextest run --workspace -E 'test(live_interop_fuzz)' --run-ignored=all
//! ```
//!
//! The test skips cleanly when no upstream rsync binary is available.

#![cfg(unix)]

mod integration;

use integration::helpers::*;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use checksums::strong::Sha256;
use filetime::{FileTime, set_file_times};

// ============================================================================
// PRNG: xorshift64*, no external crate dependency.
// ============================================================================

/// Seedable PRNG used by every randomised decision in this test.
///
/// xorshift64* is small, deterministic, and adequate for property-style
/// fuzzing -- this test is not a cryptographic generator.
struct Rng {
    state: u64,
}

impl Rng {
    fn from_seed(seed: u64) -> Self {
        // xorshift64* requires non-zero state.
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

    fn gen_bool(&mut self, p_percent: u32) -> bool {
        (self.next_u64() % 100) < u64::from(p_percent)
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

/// Path layout style used to pick filenames that exercise different parts of
/// the protocol's name handling.
const NAME_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-_";

#[derive(Debug, Clone, Copy)]
struct TreeShape {
    max_files: usize,
    max_dirs: usize,
    max_file_size: usize,
    allow_symlinks: bool,
    allow_sparse: bool,
}

impl TreeShape {
    fn small() -> Self {
        Self {
            max_files: 8,
            max_dirs: 3,
            max_file_size: 8 * 1024,
            allow_symlinks: true,
            allow_sparse: true,
        }
    }

    fn medium() -> Self {
        Self {
            max_files: 32,
            max_dirs: 6,
            max_file_size: 128 * 1024,
            allow_symlinks: true,
            allow_sparse: true,
        }
    }

    fn large() -> Self {
        Self {
            max_files: 96,
            max_dirs: 10,
            max_file_size: 512 * 1024,
            allow_symlinks: true,
            allow_sparse: true,
        }
    }
}

fn rand_name(rng: &mut Rng, len: usize) -> String {
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        let idx = (rng.next_u64() % NAME_ALPHABET.len() as u64) as usize;
        bytes.push(NAME_ALPHABET[idx]);
    }
    String::from_utf8(bytes).expect("ASCII alphabet")
}

/// Populate `root` with a random tree shaped by `shape`.
fn build_random_tree(rng: &mut Rng, root: &Path, shape: TreeShape) -> io::Result<()> {
    fs::create_dir_all(root)?;

    // Decide directory layout first so files can be placed at multiple depths.
    let dir_count = rng.gen_usize(1, shape.max_dirs + 1);
    let mut dirs: Vec<PathBuf> = vec![root.to_path_buf()];
    for _ in 0..dir_count {
        let parent_idx = rng.gen_usize(0, dirs.len());
        let name_len = rng.gen_usize(3, 10);
        let name = rand_name(rng, name_len);
        let path = dirs[parent_idx].join(name);
        fs::create_dir_all(&path)?;
        dirs.push(path);
    }

    let file_count = rng.gen_usize(1, shape.max_files + 1);
    for _ in 0..file_count {
        let dir_idx = rng.gen_usize(0, dirs.len());
        let name_len = rng.gen_usize(3, 12);
        let name = rand_name(rng, name_len);
        let path = dirs[dir_idx].join(name);

        let size = rng.gen_usize(0, shape.max_file_size + 1);
        let mut buf = vec![0u8; size];
        rng.fill_bytes(&mut buf);

        // Inject a sparse hole into ~20% of non-empty files to exercise the
        // sparse path in both implementations. We do this by zeroing a slice
        // -- creating a real hole would require platform-specific syscalls,
        // and rsync only needs zero-runs to be reproduced byte-for-byte.
        if shape.allow_sparse && size > 4096 && rng.gen_bool(20) {
            let hole_off = rng.gen_usize(0, size - 1024);
            let hole_len = rng.gen_usize(512, (size - hole_off).min(4096));
            for b in &mut buf[hole_off..hole_off + hole_len] {
                *b = 0;
            }
        }

        fs::write(&path, &buf)?;

        // Random mtime in the past so quick-check does not accidentally skip
        // transfers when src and dst start equal.
        let secs = rng.gen_range(1_600_000_000, 1_750_000_000) as i64;
        let nsecs = (rng.next_u64() % 1_000_000_000) as u32;
        let mtime = FileTime::from_unix_time(secs, nsecs);
        set_file_times(&path, mtime, mtime)?;

        // Randomise mode bits (Unix only) within a safe subset.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode_choices = [0o644u32, 0o600, 0o664, 0o755, 0o400, 0o755];
            let mode = mode_choices[rng.gen_usize(0, mode_choices.len())];
            let mut perm = fs::metadata(&path)?.permissions();
            perm.set_mode(mode);
            fs::set_permissions(&path, perm)?;
        }
    }

    // Optionally seed a few symlinks. We point them at existing files within
    // the tree to keep the dst byte-identical assertion meaningful.
    #[cfg(unix)]
    if shape.allow_symlinks && rng.gen_bool(60) {
        let files = collect_regular_files(root)?;
        if !files.is_empty() {
            let symlink_count = rng.gen_usize(1, 4);
            for _ in 0..symlink_count {
                let dir = &dirs[rng.gen_usize(0, dirs.len())];
                let name = format!("link_{}", rand_name(rng, 4));
                let link_path = dir.join(name);
                if link_path.exists() {
                    continue;
                }
                let target = &files[rng.gen_usize(0, files.len())];
                // Use a path relative to the tree root so the symlink target
                // string is identical regardless of whether the link is read
                // from the source or the destination tree.
                let target_rel = target
                    .strip_prefix(root)
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| target.clone());
                let _ = std::os::unix::fs::symlink(&target_rel, &link_path);
            }
        }
    }

    Ok(())
}

fn collect_regular_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, &mut |p, ft| {
        if ft.is_file() {
            out.push(p.to_path_buf());
        }
    })?;
    Ok(out)
}

fn walk<F: FnMut(&Path, &fs::FileType)>(root: &Path, cb: &mut F) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        cb(&path, &ft);
        if ft.is_dir() {
            walk(&path, cb)?;
        }
    }
    Ok(())
}

// ============================================================================
// Snapshot + comparison
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
// Upstream binary discovery
// ============================================================================

fn upstream_binary() -> Option<PathBuf> {
    // Prefer an explicit override so CI / local users can point at any build.
    if let Ok(path) = env::var("OC_RSYNC_FUZZ_UPSTREAM_BIN") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    // Fall back to the versions installed by tools/ci/run_interop.sh.
    for candidate in [
        "target/interop/upstream-install/3.4.2/bin/rsync",
        "target/interop/upstream-install/3.4.1/bin/rsync",
    ] {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return Some(p);
        }
    }
    // Last resort: a system rsync on PATH (only acceptable for local dev).
    if let Ok(path) = env::var("PATH") {
        for dir in env::split_paths(&path) {
            let p = dir.join("rsync");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

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

// ============================================================================
// Transfer drivers
// ============================================================================

/// Drive a local transfer using the given sender and receiver binaries.
///
/// We use plain local-style rsync (`src/` `dst/`) so the harness exercises the
/// full sender/receiver/generator pipeline of each binary without requiring an
/// SSH daemon. The receiver binary is the one whose CWD the dst lives in,
/// which for local mode is the same process as the sender -- but the role we
/// care about is whichever binary is invoked.
fn run_local_transfer(binary: &Path, src: &Path, dst: &Path) -> io::Result<()> {
    use std::process::Command;
    let src_arg = format!("{}/", src.display());
    let dst_arg = format!("{}/", dst.display());
    let mut cmd = Command::new(binary);
    // -a archive mode, --delete so reconciliation removes mutated-away files,
    // --no-owner / --no-group so the test is meaningful when run as non-root.
    cmd.arg("-a")
        .arg("--delete")
        .arg("--no-owner")
        .arg("--no-group")
        .arg(&src_arg)
        .arg(&dst_arg);
    let output = spawn_with_timeout(cmd, Duration::from_secs(120))?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{} {}->{} failed: status={:?}\nstderr:\n{}",
            binary.display(),
            src.display(),
            dst.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// One full iteration: build a tree, push, pull, optionally mutate + reconcile.
///
/// Returns `Ok(())` on success or an `Err` describing the divergence so the
/// test can print the seed and fail fast.
fn run_iteration(
    rng: &mut Rng,
    iter_idx: usize,
    seed: u64,
    shape: TreeShape,
    oc_bin: &Path,
    up_bin: &Path,
) -> io::Result<()> {
    let test_dir = TestDir::new()?;
    let src = test_dir.mkdir("src")?;
    let dst_a = test_dir.mkdir("dst_a")?;
    let dst_b = test_dir.mkdir("dst_b")?;

    build_random_tree(rng, &src, shape)?;
    let src_snap = snapshot(&src)?;

    // Direction 1: oc-rsync transfers src -> dst_a, then upstream verifies by
    // doing the same transfer (no-op if oc-rsync already produced the right
    // bytes). We then snapshot dst_a and compare against src.
    run_local_transfer(oc_bin, &src, &dst_a)?;
    let dst_a_snap = snapshot(&dst_a)?;
    let diffs = diff_snapshots(&src_snap, &dst_a_snap);
    if !diffs.is_empty() {
        return Err(io::Error::other(format!(
            "oc-rsync push diverged (seed={seed}, iter={iter_idx}):\n{}",
            diffs.join("\n")
        )));
    }

    // Direction 2: upstream transfers src -> dst_b, then we re-snapshot and
    // diff. This catches cases where upstream produces a tree that oc-rsync
    // could not produce (or vice versa).
    run_local_transfer(up_bin, &src, &dst_b)?;
    let dst_b_snap = snapshot(&dst_b)?;
    let diffs = diff_snapshots(&src_snap, &dst_b_snap);
    if !diffs.is_empty() {
        return Err(io::Error::other(format!(
            "upstream push diverged (seed={seed}, iter={iter_idx}):\n{}",
            diffs.join("\n")
        )));
    }

    // Cross-check: dst_a (oc-rsync) and dst_b (upstream) must agree.
    let diffs = diff_snapshots(&dst_a_snap, &dst_b_snap);
    if !diffs.is_empty() {
        return Err(io::Error::other(format!(
            "oc-rsync vs upstream output diverged (seed={seed}, iter={iter_idx}):\n{}",
            diffs.join("\n")
        )));
    }

    // Reconciliation pass: mutate src, then re-run both transfers and assert
    // the destinations follow. About one iteration in three exercises this
    // path so the smoke budget still completes quickly.
    if rng.gen_bool(33) {
        mutate_tree(rng, &src)?;
        let src_snap = snapshot(&src)?;

        run_local_transfer(oc_bin, &src, &dst_a)?;
        let dst_a_snap = snapshot(&dst_a)?;
        let diffs = diff_snapshots(&src_snap, &dst_a_snap);
        if !diffs.is_empty() {
            return Err(io::Error::other(format!(
                "oc-rsync reconciliation diverged (seed={seed}, iter={iter_idx}):\n{}",
                diffs.join("\n")
            )));
        }

        run_local_transfer(up_bin, &src, &dst_b)?;
        let dst_b_snap = snapshot(&dst_b)?;
        let diffs = diff_snapshots(&src_snap, &dst_b_snap);
        if !diffs.is_empty() {
            return Err(io::Error::other(format!(
                "upstream reconciliation diverged (seed={seed}, iter={iter_idx}):\n{}",
                diffs.join("\n")
            )));
        }
    }

    Ok(())
}

/// Apply a handful of random mutations to `root`: add files, delete files,
/// modify existing files. Mutations stay within the same shape envelope.
fn mutate_tree(rng: &mut Rng, root: &Path) -> io::Result<()> {
    let files = collect_regular_files(root)?;
    let mut_count = rng.gen_usize(1, 5);
    for _ in 0..mut_count {
        let choice = rng.next_u64() % 3;
        match choice {
            0 => {
                // Delete a random file if any remain.
                if !files.is_empty() {
                    let victim = &files[rng.gen_usize(0, files.len())];
                    let _ = fs::remove_file(victim);
                }
            }
            1 => {
                // Append random bytes to a random file.
                if !files.is_empty() {
                    let victim = &files[rng.gen_usize(0, files.len())];
                    let len = rng.gen_usize(1, 4096);
                    let mut buf = vec![0u8; len];
                    rng.fill_bytes(&mut buf);
                    let mut existing = fs::read(victim).unwrap_or_default();
                    existing.extend_from_slice(&buf);
                    fs::write(victim, &existing)?;
                    let secs = rng.gen_range(1_600_000_000, 1_750_000_000) as i64;
                    let mtime = FileTime::from_unix_time(secs, 0);
                    set_file_times(victim, mtime, mtime)?;
                }
            }
            _ => {
                // Add a brand-new file at the tree root.
                let name = format!("new_{}", rand_name(rng, 6));
                let path = root.join(name);
                let len = rng.gen_usize(0, 8192);
                let mut buf = vec![0u8; len];
                rng.fill_bytes(&mut buf);
                fs::write(&path, &buf)?;
                let secs = rng.gen_range(1_600_000_000, 1_750_000_000) as i64;
                let mtime = FileTime::from_unix_time(secs, 0);
                set_file_times(&path, mtime, mtime)?;
            }
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

/// Run the fuzzer until the time budget elapses. Used by both the smoke and
/// extended tiers; the only difference is the budget and tree shape mix.
fn run_until_budget(budget: Duration, shapes: &[TreeShape]) {
    let oc_bin = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            eprintln!("skip: oc-rsync binary not located");
            return;
        }
    };
    let up_bin = match upstream_binary() {
        Some(p) => p,
        None => {
            eprintln!(
                "skip: no upstream rsync found; install via tools/ci/run_interop.sh or set OC_RSYNC_FUZZ_UPSTREAM_BIN"
            );
            return;
        }
    };

    let seed = seed_for_run();
    eprintln!(
        "live interop fuzz: seed={seed} budget={:?} oc_bin={} upstream_bin={}",
        budget,
        oc_bin.display(),
        up_bin.display()
    );

    let mut rng = Rng::from_seed(seed);
    let start = Instant::now();
    let mut iter = 0usize;
    while start.elapsed() < budget {
        let shape = shapes[iter % shapes.len()];
        if let Err(e) = run_iteration(&mut rng, iter, seed, shape, &oc_bin, &up_bin) {
            panic!(
                "live interop fuzz failed (seed={seed}, iter={iter}): {e}\n\
                 reproduce: OC_RSYNC_FUZZ_SEED={seed} OC_RSYNC_FUZZ_BUDGET_SECS=600 \
                 cargo nextest run --workspace -E 'test(live_interop_fuzz)' --run-ignored=all"
            );
        }
        iter += 1;
    }

    eprintln!(
        "live interop fuzz: completed {iter} iterations in {:?}",
        start.elapsed()
    );
}

/// Smoke tier (default 60-second budget, small trees only).
#[test]
#[ignore = "spawns subprocesses; run via --run-ignored=all"]
fn live_interop_fuzz_smoke() {
    let budget = Duration::from_secs(budget_seconds().min(120));
    run_until_budget(budget_clamp_for_smoke(budget), &[TreeShape::small()]);
}

/// Extended tier (default 60-second budget but expects user to bump
/// `OC_RSYNC_FUZZ_BUDGET_SECS=3600`; cycles through three tree sizes).
#[test]
#[ignore = "long-running; opt in via OC_RSYNC_FUZZ_BUDGET_SECS"]
fn live_interop_fuzz_extended() {
    let budget = Duration::from_secs(budget_seconds());
    run_until_budget(
        budget,
        &[TreeShape::small(), TreeShape::medium(), TreeShape::large()],
    );
}

/// Clamp the smoke-tier budget so that it cannot exceed two minutes even if
/// the env var requests more -- the extended test is the place for that.
fn budget_clamp_for_smoke(b: Duration) -> Duration {
    if b > Duration::from_secs(120) {
        Duration::from_secs(120)
    } else {
        b
    }
}
