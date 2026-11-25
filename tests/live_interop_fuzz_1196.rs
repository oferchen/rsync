//! Live interop fuzzer (#1196): randomised transfers between oc-rsync and
//! the upstream rsync 3.5.2 oracle.
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
//! 2. Push: oc-rsync client sender, upstream server receiver -- verify dst
//!    hashes match src.
//! 3. Pull: upstream server sender, oc-rsync client receiver -- verify dst
//!    hashes match src.
//! 4. Optionally mutate the source (add/delete/modify files) and re-run both
//!    transfers to assert that the second pass reconciles correctly.
//!
//! The upstream end runs as the `--server` of a remote-shell transfer: the
//! `-e` command drops the host name and runs the server command locally, and
//! `--rsync-path` names the oracle binary.
//!
//! ## Reproducibility
//!
//! All randomness flows through a single seedable PRNG (xorshift64*). The seed
//! defaults to a value derived from the system clock but can be pinned via the
//! `OC_RSYNC_FUZZ_SEED` environment variable. The seed is printed at the start
//! of every run and in every failure, so the run can be reproduced exactly.
//!
//! ## Iterations
//!
//! Each tier runs a fixed number of iterations; there is no time budget.
//! `OC_RSYNC_FUZZ_ITERATIONS` overrides the count. The tests are `#[ignore]`
//! because they spawn hundreds of subprocesses:
//!
//! - Smoke tier: 16 iterations of small trees -- run by the nightly CI cell.
//! - Extended tier: 48 iterations cycling through small, medium and large
//!   trees.
//!
//! ## Invocation
//!
//! ```text
//! cargo nextest run -p bin --test live_interop_fuzz_1196 --run-ignored=all
//! ```
//!
//! The oracle comes from `test_support::require_upstream_rsync`: without it
//! the tests skip, and under `OC_RSYNC_UPSTREAM_COMPAT=1` they fail.

#![cfg(unix)]

mod integration;

use integration::helpers::*;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use checksums::strong::Sha256;
use filetime::FileTime;
use test_support::file_times::set_file_times;
use test_support::{UpstreamVersion, require_upstream_rsync};

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

/// Locates the binary under test.
///
/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so it must be read with
/// `env!`, not `env::var_os`: at run time it is unset and the lookup would fall
/// through to whatever stale `target/debug/oc-rsync` happens to be on disk -
/// silently testing a different build than the one just compiled.
fn locate_oc_rsync() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

// ============================================================================
// Transfer drivers
// ============================================================================

/// The two binaries of a transfer: the oc-rsync client and the upstream
/// server it reaches through `rsh`.
struct Peers<'a> {
    oc: &'a Path,
    upstream: &'a Path,
    rsh: &'a Path,
}

/// Transfer direction relative to the oc-rsync client.
#[derive(Clone, Copy)]
enum Direction {
    /// oc-rsync sends `src`; the upstream server receives into `dst`.
    Push,
    /// The upstream server sends `src`; oc-rsync receives into `dst`.
    Pull,
}

/// Writes the remote-shell stand-in: it drops the host argument and runs the
/// server command line locally.
fn write_rsh(dir: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let rsh = dir.join("rsh.sh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n")?;
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755))?;
    Ok(rsh)
}

/// Runs one oc-rsync client transfer against the upstream server.
fn run_transfer(peers: &Peers<'_>, direction: Direction, src: &Path, dst: &Path) -> io::Result<()> {
    use std::process::Command;
    let local = |path: &Path| format!("{}/", path.display());
    let remote = |path: &Path| format!("peer:{}/", path.display());
    let (src_arg, dst_arg) = match direction {
        Direction::Push => (local(src), remote(dst)),
        Direction::Pull => (remote(src), local(dst)),
    };
    let mut cmd = Command::new(peers.oc);
    // -a archive mode, --delete so reconciliation removes mutated-away files,
    // --no-owner / --no-group so the test is meaningful when run as non-root.
    cmd.arg("-a")
        .arg("--delete")
        .arg("--no-owner")
        .arg("--no-group")
        .arg("-e")
        .arg(peers.rsh)
        .arg(format!("--rsync-path={}", peers.upstream.display()))
        .arg(&src_arg)
        .arg(&dst_arg);
    let output = spawn_with_timeout(cmd, Duration::from_secs(120))?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{} {src_arg} -> {dst_arg} failed: status={:?}\nstderr:\n{}",
            peers.oc.display(),
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
    let dst_push = test_dir.mkdir("dst_push")?;
    let dst_pull = test_dir.mkdir("dst_pull")?;
    let rsh = write_rsh(test_dir.path())?;
    let peers = Peers {
        oc: oc_bin,
        upstream: up_bin,
        rsh: &rsh,
    };

    build_random_tree(rng, &src, shape)?;

    // Each pass pushes src to dst_push and pulls it into dst_pull, then
    // checks that both destinations equal the source.
    let pass = |stage: &str| -> io::Result<()> {
        let src_snap = snapshot(&src)?;
        for (direction, dst, label) in [
            (Direction::Push, &dst_push, "push to upstream"),
            (Direction::Pull, &dst_pull, "pull from upstream"),
        ] {
            run_transfer(&peers, direction, &src, dst)?;
            let diffs = diff_snapshots(&src_snap, &snapshot(dst)?);
            if !diffs.is_empty() {
                return Err(io::Error::other(format!(
                    "{stage} {label} diverged (seed={seed}, iter={iter_idx}):\n{}",
                    diffs.join("\n")
                )));
            }
        }
        Ok(())
    };

    pass("initial")?;

    // Reconciliation pass: mutate src, then re-run both transfers and assert
    // the destinations follow. About one iteration in three exercises this
    // path.
    if rng.gen_bool(33) {
        mutate_tree(rng, &src)?;
        pass("reconciliation")?;
    }

    Ok(())
}

/// Apply a handful of random mutations to `root`: add files, delete files,
/// modify existing files. Mutations stay within the same shape envelope.
fn mutate_tree(rng: &mut Rng, root: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
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
                    // The tree builder hands out 0o400 files; lift owner write
                    // for the append and restore the mode afterwards.
                    let mode = fs::metadata(victim).map(|m| m.permissions().mode()).ok();
                    if let Some(mode) = mode {
                        fs::set_permissions(victim, fs::Permissions::from_mode(mode | 0o200))?;
                    }
                    fs::write(victim, &existing)?;
                    if let Some(mode) = mode {
                        fs::set_permissions(victim, fs::Permissions::from_mode(mode))?;
                    }
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

/// Iteration count: `OC_RSYNC_FUZZ_ITERATIONS` when set, else the tier's
/// default.
fn iterations(default: usize) -> usize {
    env::var("OC_RSYNC_FUZZ_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn seed_for_run() -> u64 {
    if let Ok(s) = env::var("OC_RSYNC_FUZZ_SEED")
        && let Ok(v) = s.parse()
    {
        return v;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xC0FF_EE12_3456_789A)
}

/// Runs `count` iterations, cycling through `shapes`. Used by both tiers; the
/// only difference is the count and tree shape mix.
fn run_iterations(test: &str, count: usize, shapes: &[TreeShape]) {
    let oc_bin = locate_oc_rsync();
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up_bin = upstream.binary();

    let seed = seed_for_run();
    eprintln!(
        "live interop fuzz: seed={seed} iterations={count} oc_bin={} upstream_bin={}",
        oc_bin.display(),
        up_bin.display()
    );

    let mut rng = Rng::from_seed(seed);
    for iter in 0..count {
        let shape = shapes[iter % shapes.len()];
        if let Err(e) = run_iteration(&mut rng, iter, seed, shape, &oc_bin, up_bin) {
            panic!(
                "live interop fuzz failed (seed={seed}, iter={iter}): {e}\n\
                 reproduce: OC_RSYNC_FUZZ_SEED={seed} OC_RSYNC_FUZZ_ITERATIONS={} \
                 cargo nextest run -p bin --test live_interop_fuzz_1196 \
                 --run-ignored=all -E 'test(={test})'",
                iter + 1
            );
        }
    }
}

/// Smoke tier: 16 iterations of small trees.
#[test]
#[ignore = "spawns subprocesses; run via --run-ignored=all"]
fn live_interop_fuzz_smoke() {
    run_iterations(
        "live_interop_fuzz_smoke",
        iterations(16),
        &[TreeShape::small()],
    );
}

/// Extended tier: 48 iterations cycling through three tree sizes.
#[test]
#[ignore = "long-running; opt in with --run-ignored=all"]
fn live_interop_fuzz_extended() {
    run_iterations(
        "live_interop_fuzz_extended",
        iterations(48),
        &[TreeShape::small(), TreeShape::medium(), TreeShape::large()],
    );
}
