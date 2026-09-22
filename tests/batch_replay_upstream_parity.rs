//! Cross-implementation parity suite for `--read-batch` replay.
//!
//! Upstream's batch model (batch.c, main.c:639-651): `--write-batch` records
//! the stream-flags bitmap, the file list and every delta the receiver
//! requested; `--read-batch` replays the file as `f_in` through the ordinary
//! receiver with a local generator on a pipe. Equivalence therefore has two
//! directions, and one alone cannot catch an asymmetric encoding bug:
//!
//! - a batch recorded by upstream must replay identically under oc, and
//! - a batch recorded by oc must replay identically under upstream.
//!
//! Every cell here records the SAME fixture with BOTH implementations, then
//! replays each batch with BOTH implementations into identical seeded
//! destinations. The oracle is upstream's own replay of the same bytes -
//! never a hardcoded expectation.
//!
//! Oracle location follows the root-test convention: `OC_RSYNC_UPSTREAM_RSYNC`
//! override, then `target/interop/upstream-install/<v>/bin/rsync`, then the
//! `target/interop/upstream-src/rsync-<v>/rsync` build tree, banner-verified.
//! When no oracle resolves the tests early-return with a printed reason; when
//! one resolves, each cell asserts the oracle's own recording ran (exit 0),
//! so a green run cannot be a silent self-skip.
//!
//! Known divergences found by this suite are pinned as `#[ignore = "..."]`
//! tests so the parity that DOES hold stays enforced while the failures stay
//! visible; each ignore reason names the divergence.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use filetime::FileTime;

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Fixed mtimes: seeds are backdated a year behind sources so the quick
/// check (size + mtime) can never silently skip a cell's transfer.
const SEED_MTIME: i64 = 1_500_000_000;
const SRC_MTIME: i64 = 1_600_000_000;

/// Locates the binary under test.
///
/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so it must be read
/// with `env!`: at run time it is unset and a lookup would fall through to
/// whatever stale `target/debug/oc-rsync` happens to be on disk.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// Resolves a genuine upstream rsync to serve as the oracle.
///
/// Returns the path plus the `--version` banner line, which the caller has
/// therefore positively observed running - a path alone proves nothing
/// (macOS `/usr/bin/rsync` is openrsync and shares none of the batch
/// format).
fn locate_upstream_rsync() -> Option<(PathBuf, String)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC") {
        candidates.push(PathBuf::from(explicit));
    }
    for version in ["3.5.0", "3.4.4"] {
        if let Some(installed) = test_support::upstream_install_bin(version) {
            candidates.push(installed);
        }
        if let Some(root) = test_support::workspace_root() {
            candidates.push(
                root.join("target")
                    .join("interop")
                    .join("upstream-src")
                    .join(format!("rsync-{version}"))
                    .join("rsync"),
            );
        }
    }
    candidates.into_iter().find_map(|candidate| {
        let out = Command::new(&candidate).arg("--version").output().ok()?;
        let banner = String::from_utf8(out.stdout).ok()?;
        let first = banner.lines().next()?;
        (first.starts_with("rsync") && first.contains(" version 3."))
            .then(|| (candidate, first.to_string()))
    })
}

fn spawn_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Drain both pipes on their own threads; polling try_wait() while the
    // pipes fill would deadlock once the child blocks on a full OS buffer.
    let mut child_stdout = child.stdout.take()?;
    let mut child_stderr = child.stderr.take()?;
    let stdout_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                return Some(Output {
                    status,
                    stdout: stdout_reader.join().unwrap_or_default(),
                    stderr: stderr_reader.join().unwrap_or_default(),
                });
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// Stamps `mtime` on every entry under `root` (children first, so directory
/// mtimes survive their own content writes).
fn stamp_tree_mtime(root: &Path, mtime: i64) {
    let ft = FileTime::from_unix_time(mtime, 0);
    let meta = fs::symlink_metadata(root).expect("stat fixture entry");
    if meta.is_dir() {
        for entry in fs::read_dir(root).expect("read fixture dir") {
            stamp_tree_mtime(&entry.expect("dir entry").path(), mtime);
        }
    }
    filetime::set_symlink_file_times(root, ft, ft).expect("stamp fixture mtime");
}

/// Copies `from` into `to` preserving contents, modes and mtimes (dirs
/// stamped after their children). Seeds contain no symlinks or hardlinks,
/// so plain file copies are sufficient.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create tree copy root");
    for entry in fs::read_dir(from).expect("read tree") {
        let entry = entry.expect("tree entry");
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let meta = fs::symlink_metadata(&src).expect("stat tree entry");
        if meta.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).expect("copy tree file");
            let ft = FileTime::from_last_modification_time(&meta);
            filetime::set_file_times(&dst, ft, ft).expect("stamp copied mtime");
        }
    }
    let meta = fs::metadata(from).expect("stat tree root");
    let ft = FileTime::from_last_modification_time(&meta);
    filetime::set_file_times(to, ft, ft).expect("stamp copied dir mtime");
}

/// One comparable line per filesystem entry: kind, mode, mtime, content,
/// and for regular files with nlink > 1 the hardlink group, expressed as
/// the lexicographically first path sharing the inode (inode numbers
/// themselves differ across replays and must not leak into the snapshot).
fn snapshot_tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
        for entry in fs::read_dir(dir).expect("read snapshot dir") {
            let path = entry.expect("snapshot entry").path();
            let rel = path
                .strip_prefix(root)
                .expect("snapshot rel path")
                .to_string_lossy()
                .into_owned();
            if fs::symlink_metadata(&path).expect("stat").is_dir() {
                walk(root, &path, out);
            }
            out.push((rel, path));
        }
    }
    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries.sort();

    let mut inode_group: BTreeMap<(u64, u64), String> = BTreeMap::new();
    let mut snapshot = BTreeMap::new();
    for (rel, path) in entries {
        let meta = fs::symlink_metadata(&path).expect("stat snapshot entry");
        let mode = meta.mode() & 0o7777;
        let mtime = meta.mtime();
        let line = if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).expect("read snapshot symlink");
            format!("symlink -> {}", target.display())
        } else if meta.is_dir() {
            format!("dir mode={mode:o} mtime={mtime}")
        } else {
            let data = fs::read(&path).expect("read snapshot file");
            let group = if meta.nlink() > 1 {
                inode_group
                    .entry((meta.dev(), meta.ino()))
                    .or_insert_with(|| rel.clone())
                    .clone()
            } else {
                String::new()
            };
            format!(
                "file mode={mode:o} mtime={mtime} nlink={} group={group} bytes={data:?}",
                meta.nlink()
            )
        };
        snapshot.insert(rel, line);
    }
    snapshot
}

fn run_rsync(bin: &Path, args: &[&str], operands: &[String]) -> Output {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.args(operands);
    spawn_with_timeout(cmd, RUN_TIMEOUT)
        .unwrap_or_else(|| panic!("{} did not finish within the timeout", bin.display()))
}

/// One replay observation: which binary replayed, into which tree, saying
/// what.
struct Replay {
    label: String,
    output: Output,
    tree: BTreeMap<String, String>,
}

/// The full parity matrix for one cell: each recorded batch (upstream's and
/// oc's) replayed by both implementations onto identical seeded dests.
struct ParityMatrix {
    /// (batch label, oc replay, upstream replay) per recorded batch.
    cells: Vec<(String, Replay, Replay)>,
}

struct ParityHarness {
    temp: tempfile::TempDir,
    oc: PathBuf,
    upstream: PathBuf,
    src: PathBuf,
    seed: PathBuf,
}

impl ParityHarness {
    /// Builds the fixture and resolves both binaries. `None` means the
    /// oracle is unavailable; the reason has been printed.
    fn new(build_src: &dyn Fn(&Path), build_seed: &dyn Fn(&Path)) -> Option<Self> {
        let Some((upstream, banner)) = locate_upstream_rsync() else {
            eprintln!(
                "Skipping batch replay parity test: no upstream rsync oracle found \
                 (set OC_RSYNC_UPSTREAM_RSYNC or install one under \
                 target/interop/upstream-install/)"
            );
            return None;
        };
        eprintln!(
            "batch replay parity oracle: {} ({banner})",
            upstream.display()
        );

        let temp = tempfile::TempDir::new().expect("create parity temp dir");
        let src = temp.path().join("src");
        let seed = temp.path().join("seed");
        fs::create_dir_all(&src).expect("create src");
        fs::create_dir_all(&seed).expect("create seed");
        build_src(&src);
        build_seed(&seed);
        stamp_tree_mtime(&src, SRC_MTIME);
        stamp_tree_mtime(&seed, SEED_MTIME);

        Some(Self {
            temp,
            oc: oc_rsync_binary(),
            upstream,
            src,
            seed,
        })
    }

    /// Records the fixture with both implementations and replays each batch
    /// with both. Recording exits are asserted here: a failed upstream
    /// recording would otherwise turn every later comparison vacuous.
    fn run(&self, record_flags: &[&str], replay_flags: &[&str]) -> ParityMatrix {
        let mut cells = Vec::new();
        for (producer_label, producer) in
            [("upstream-batch", &self.upstream), ("oc-batch", &self.oc)]
        {
            let batch = self.temp.path().join(format!("{producer_label}.batch"));
            let record_dest = self.temp.path().join(format!("{producer_label}-record"));
            copy_tree(&self.seed, &record_dest);
            let mut args = vec!["-a"];
            args.extend_from_slice(record_flags);
            let batch_flag = format!("--write-batch={}", batch.display());
            args.push(&batch_flag);
            let out = run_rsync(
                producer,
                &args,
                &[
                    format!("{}/", self.src.display()),
                    format!("{}/", record_dest.display()),
                ],
            );
            assert!(
                out.status.success(),
                "{producer_label}: recording must exit 0, got {:?}\nstderr: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                fs::metadata(&batch).map(|m| m.len()).unwrap_or(0) > 0,
                "{producer_label}: a non-empty batch file must be produced"
            );

            let oc_replay =
                self.replay(&self.oc, "oc-replay", producer_label, &batch, replay_flags);
            let up_replay = self.replay(
                &self.upstream,
                "upstream-replay",
                producer_label,
                &batch,
                replay_flags,
            );
            cells.push((producer_label.to_string(), oc_replay, up_replay));
        }
        ParityMatrix { cells }
    }

    fn replay(
        &self,
        bin: &Path,
        replayer: &str,
        producer: &str,
        batch: &Path,
        replay_flags: &[&str],
    ) -> Replay {
        let dest = self.temp.path().join(format!("{producer}-{replayer}"));
        copy_tree(&self.seed, &dest);
        let mut args = vec!["-a"];
        args.extend_from_slice(replay_flags);
        let batch_flag = format!("--read-batch={}", batch.display());
        args.push(&batch_flag);
        let output = run_rsync(bin, &args, &[format!("{}/", dest.display())]);
        Replay {
            label: format!("{producer}/{replayer}"),
            output,
            tree: snapshot_tree(&dest),
        }
    }

    /// Replays `batch` with both implementations onto trees that already
    /// hold the fully updated state (the up-to-date cell).
    fn replay_up_to_date(&self, replay_flags: &[&str]) -> (Replay, Replay) {
        let batch = self.temp.path().join("upstream-batch.batch");
        let updated = self.temp.path().join("upstream-batch-record");
        assert!(
            batch.is_file() && updated.is_dir(),
            "run() must record the upstream batch before an up-to-date replay"
        );
        let mut replays = Vec::new();
        for (replayer, bin) in [("oc-replay", &self.oc), ("upstream-replay", &self.upstream)] {
            let dest = self.temp.path().join(format!("uptodate-{replayer}"));
            copy_tree(&updated, &dest);
            let mut args = vec!["-a"];
            args.extend_from_slice(replay_flags);
            let batch_flag = format!("--read-batch={}", batch.display());
            args.push(&batch_flag);
            let output = run_rsync(bin, &args, &[format!("{}/", dest.display())]);
            replays.push(Replay {
                label: format!("uptodate/{replayer}"),
                output,
                tree: snapshot_tree(&dest),
            });
        }
        let upstream = replays.pop().expect("upstream up-to-date replay");
        let oc = replays.pop().expect("oc up-to-date replay");
        (oc, upstream)
    }
}

/// Asserts exit-code and destination-tree parity for every cell of the
/// matrix, plus cross-batch equality (identical fixture, so all four final
/// trees must coincide).
fn assert_tree_parity(matrix: &ParityMatrix) {
    let mut reference: Option<(&str, &BTreeMap<String, String>)> = None;
    for (batch_label, oc, upstream) in &matrix.cells {
        assert_eq!(
            oc.output.status.code(),
            upstream.output.status.code(),
            "{batch_label}: replay exit codes diverge (oc stderr: {} | upstream stderr: {})",
            String::from_utf8_lossy(&oc.output.stderr),
            String::from_utf8_lossy(&upstream.output.stderr)
        );
        assert_eq!(
            oc.tree, upstream.tree,
            "{batch_label}: oc and upstream replays produced different destination trees"
        );
        match &reference {
            None => reference = Some((batch_label, &upstream.tree)),
            Some((ref_label, ref_tree)) => assert_eq!(
                &&upstream.tree, ref_tree,
                "replays of {batch_label} and {ref_label} diverge on the same fixture"
            ),
        }
    }
}

fn stdout_lines(replay: &Replay) -> Vec<String> {
    String::from_utf8_lossy(&replay.output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn build_basic_src(src: &Path) {
    fs::create_dir_all(src.join("sub")).expect("create src/sub");
    fs::write(src.join("alpha.txt"), b"alpha v2 payload\n").expect("write alpha");
    fs::write(src.join("sub/beta.txt"), vec![b'b'; 40_000]).expect("write beta");
    fs::write(src.join("fresh.txt"), b"fresh file\n").expect("write fresh");
}

fn build_basic_seed(seed: &Path) {
    // Same name, different content: forces a genuine delta transfer.
    fs::write(seed.join("alpha.txt"), b"alpha v1 stale contents\n").expect("write stale alpha");
}

// ---------------------------------------------------------------------------
// Tree-parity cells
// ---------------------------------------------------------------------------

/// Baseline: a mixed tree (fresh file, stale basis, subdirectory) recorded
/// and replayed in all four producer x replayer combinations must yield one
/// identical destination tree and identical exit codes.
#[test]
fn replay_tree_parity_matches_upstream() {
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_basic_seed) else {
        return;
    };
    let matrix = harness.run(&[], &[]);
    assert_tree_parity(&matrix);
}

/// `--delete` on replay must reproduce upstream's deletion set: extras in
/// the destination that are absent from the recorded file list disappear,
/// on both batch directions. Upstream runs the deletion from the LOCAL
/// generator at replay time, so `--delete` is passed to the replays too.
#[test]
fn replay_delete_parity_matches_upstream() {
    let build_seed = |seed: &Path| {
        build_basic_seed(seed);
        fs::write(seed.join("extra.txt"), b"delete me\n").expect("write extra");
        fs::create_dir_all(seed.join("extra_dir")).expect("create extra dir");
        fs::write(seed.join("extra_dir/inner.txt"), b"delete me too\n").expect("write inner");
    };
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_seed) else {
        return;
    };
    let matrix = harness.run(&["--delete"], &["--delete"]);
    for (_, oc, _) in &matrix.cells {
        assert!(
            !oc.tree.contains_key("extra.txt") && !oc.tree.contains_key("extra_dir"),
            "{}: the deletion pass must remove extraneous entries; tree: {:?}",
            oc.label,
            oc.tree.keys().collect::<Vec<_>>()
        );
    }
    assert_tree_parity(&matrix);
}

/// `-H` replay must materialize hardlink identity the way upstream does:
/// leader and follower both exist and share one inode (nlink 2).
#[test]
#[ignore = "known divergence: oc --read-batch -aH omits the hardlink leader from the \
            destination (exit 0) and writes the follower as an unlinked regular file, \
            while upstream recreates leader + follower sharing one inode"]
fn replay_hardlink_parity_matches_upstream() {
    let build_src = |src: &Path| {
        fs::write(src.join("leader.txt"), b"linked payload\n").expect("write leader");
        fs::hard_link(src.join("leader.txt"), src.join("follower.txt")).expect("link follower");
        fs::write(src.join("solo.txt"), b"unlinked payload\n").expect("write solo");
    };
    let Some(harness) = ParityHarness::new(&build_src, &|_seed| {}) else {
        return;
    };
    let matrix = harness.run(&["-H"], &["-H"]);
    for (batch_label, _, upstream) in &matrix.cells {
        let leader = upstream
            .tree
            .get("leader.txt")
            .unwrap_or_else(|| panic!("{batch_label}: upstream replay must produce leader.txt"));
        assert!(
            leader.contains("nlink=2"),
            "{batch_label}: upstream replay must hardlink the pair; got {leader}"
        );
    }
    assert_tree_parity(&matrix);
}

// ---------------------------------------------------------------------------
// Output cells
// ---------------------------------------------------------------------------

/// Itemize rows for transferred FILES must match upstream byte for byte, in
/// order, on both batch directions.
#[test]
fn replay_itemize_file_rows_match_upstream() {
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_basic_seed) else {
        return;
    };
    let matrix = harness.run(&[], &["-i"]);
    let file_rows = |replay: &Replay| -> Vec<String> {
        stdout_lines(replay)
            .into_iter()
            .filter(|line| matches!(line.as_bytes().get(1), Some(b'f') | Some(b'L')))
            .collect()
    };
    for (batch_label, oc, upstream) in &matrix.cells {
        let oc_rows = file_rows(oc);
        assert_eq!(
            oc_rows,
            file_rows(upstream),
            "{batch_label}: itemized file rows diverge"
        );
        assert!(
            !oc_rows.is_empty(),
            "{batch_label}: the fixture must itemize at least one file row"
        );
    }
    assert_tree_parity(&matrix);
}

/// The FULL `-i` output must match, directory rows included. Upstream
/// itemizes `./` and created subdirectories on replay.
#[test]
#[ignore = "known divergence: oc --read-batch -i omits the directory rows upstream \
            prints (.d..t...... ./ and cd+++++++++ sub/); file rows already match"]
fn replay_itemize_directory_rows_match_upstream() {
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_basic_seed) else {
        return;
    };
    let matrix = harness.run(&[], &["-i"]);
    for (batch_label, oc, upstream) in &matrix.cells {
        assert_eq!(
            stdout_lines(oc),
            stdout_lines(upstream),
            "{batch_label}: full itemize output diverges"
        );
    }
}

/// The transfer-shaped `--stats` totals must match upstream: deleted
/// counts, transferred counts and the literal/matched byte split. Timing
/// lines and transfer rates are inherently host-dependent and are not
/// compared; the file-count, created-count and byte-accounting lines with
/// known divergences are pinned separately below.
#[test]
fn replay_stats_transfer_totals_match_upstream() {
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_basic_seed) else {
        return;
    };
    let matrix = harness.run(&[], &["--stats"]);
    const STABLE_PREFIXES: &[&str] = &[
        "Number of deleted files:",
        "Number of regular files transferred:",
        "Total file size:",
        "Total transferred file size:",
        "Literal data:",
        "Matched data:",
    ];
    let stable_lines = |replay: &Replay| -> Vec<String> {
        stdout_lines(replay)
            .into_iter()
            .filter(|line| STABLE_PREFIXES.iter().any(|p| line.starts_with(p)))
            .collect()
    };
    for (batch_label, oc, upstream) in &matrix.cells {
        let oc_lines = stable_lines(oc);
        assert_eq!(
            oc_lines,
            stable_lines(upstream),
            "{batch_label}: --stats transfer totals diverge"
        );
        assert_eq!(
            oc_lines.len(),
            STABLE_PREFIXES.len(),
            "{batch_label}: every compared stats line must be present; got {oc_lines:?}"
        );
    }
    assert_tree_parity(&matrix);
}

/// The remaining `--stats` lines must match too: the flist entry counts,
/// the created-files count and the wire byte accounting.
#[test]
#[ignore = "known divergence: oc --read-batch --stats undercounts Number of files \
            (reg one short of upstream), omits the File list generation/transfer \
            time lines, reports different Total bytes sent/received accounting, and \
            an oc-recorded batch makes upstream count 0 created files"]
fn replay_stats_file_counts_and_bytes_match_upstream() {
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_basic_seed) else {
        return;
    };
    let matrix = harness.run(&[], &["--stats"]);
    const PINNED_PREFIXES: &[&str] = &[
        "Number of files:",
        "Number of created files:",
        "File list generation time:",
        "File list transfer time:",
        "Total bytes sent:",
        "Total bytes received:",
    ];
    let pinned_lines = |replay: &Replay| -> Vec<String> {
        stdout_lines(replay)
            .into_iter()
            .filter(|line| PINNED_PREFIXES.iter().any(|p| line.starts_with(p)))
            .collect()
    };
    for (batch_label, oc, upstream) in &matrix.cells {
        assert_eq!(
            pinned_lines(oc),
            pinned_lines(upstream),
            "{batch_label}: --stats file counts / byte accounting diverge"
        );
    }
}

/// Writer fidelity: upstream must see the SAME transfer stream whether the
/// batch was recorded by upstream or by oc, so upstream's replay of oc's
/// batch must itemize exactly like upstream's replay of its own batch.
/// This is the one comparison that can catch a recording defect the final
/// trees hide (both replays of a degraded batch degrade identically).
#[test]
#[ignore = "known divergence: oc --write-batch records bare transfer iflags, so \
            upstream replaying an oc-recorded batch itemizes >f......... instead \
            of >f+++++++++/>f.st...... and counts 0 created files"]
fn recorded_batch_itemizes_identically_under_upstream() {
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_basic_seed) else {
        return;
    };
    let matrix = harness.run(&[], &["-i"]);
    let upstream_outputs: Vec<(String, Vec<String>)> = matrix
        .cells
        .iter()
        .map(|(batch_label, _, upstream)| (batch_label.clone(), stdout_lines(upstream)))
        .collect();
    let [(up_label, up_lines), (oc_label, oc_lines)] = upstream_outputs.as_slice() else {
        panic!("expected exactly two recorded batches");
    };
    assert_eq!(
        oc_lines, up_lines,
        "upstream replay itemizes {oc_label} differently from {up_label}: \
         the recorded streams are not equivalent"
    );
}

// ---------------------------------------------------------------------------
// Up-to-date (no-op) replay
// ---------------------------------------------------------------------------

/// Re-applying a batch to an already-updated destination must be a no-op on
/// both implementations: exit 0, destination byte-identical before and
/// after, and both replays agreeing.
#[test]
fn replay_up_to_date_is_a_noop_on_both() {
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_basic_seed) else {
        return;
    };
    let matrix = harness.run(&[], &[]);
    assert_tree_parity(&matrix);
    let baseline = snapshot_tree(&harness.temp.path().join("upstream-batch-record"));

    let (oc, upstream) = harness.replay_up_to_date(&[]);
    for replay in [&oc, &upstream] {
        assert!(
            replay.output.status.success(),
            "{}: an up-to-date replay must exit 0, got {:?}\nstderr: {}",
            replay.label,
            replay.output.status.code(),
            String::from_utf8_lossy(&replay.output.stderr)
        );
        assert_eq!(
            replay.tree, baseline,
            "{}: an up-to-date replay must leave the destination untouched",
            replay.label
        );
    }
}

/// Upstream announces every batched update its local generator no longer
/// wants: `(Skipping batched update for "NAME")` per receiver.c:964-975,
/// printed at default verbosity with exit 0.
#[test]
#[ignore = "known divergence: oc --read-batch is silent on an up-to-date replay while \
            upstream prints one (Skipping batched update for \"NAME\") line per \
            recorded file (receiver.c:964-975); exit codes and trees already match"]
fn replay_up_to_date_skipping_notices_match_upstream() {
    let Some(harness) = ParityHarness::new(&build_basic_src, &build_basic_seed) else {
        return;
    };
    let _ = harness.run(&[], &[]);
    let (oc, upstream) = harness.replay_up_to_date(&[]);
    let notices = |replay: &Replay| -> Vec<String> {
        stdout_lines(replay)
            .into_iter()
            .filter(|line| line.contains("Skipping batched update"))
            .collect()
    };
    let upstream_notices = notices(&upstream);
    assert!(
        !upstream_notices.is_empty(),
        "upstream must announce at least one skipped batched update"
    );
    assert_eq!(
        notices(&oc),
        upstream_notices,
        "up-to-date replay skip notices diverge"
    );
}
