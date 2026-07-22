//! ISI.d - multi-segment INC_RECURSE push interop (deep tree).
//!
//! Companion to ISI.c. Where ISI.c proved that a single flat directory
//! traverses the INC_RECURSE sender path end-to-end against an upstream
//! rsync 3.4.1 receiver, this test forces the sender to emit **multiple**
//! sub-list segments mid-transfer by giving it a deep nested tree.
//!
//! ## Why "multi-segment"
//!
//! Upstream `flist.c:1820 send_directory()` walks one directory per call
//! and `flist.c:2104 send_extra_file_list()` writes one
//! `NDX_FLIST_OFFSET - dir_ndx` header per sub-list, terminating the
//! whole flist stream with `write_ndx(f, NDX_FLIST_EOF)` only after every
//! directory has been dispatched (`flist.c:2172`). Our generator side
//! mirrors this in `crates/transfer/src/generator/file_list/inc_recurse.rs::
//! partition_file_list_for_inc_recurse`, which pushes one `DirSegment`
//! per subdirectory and lets `SegmentScheduler` flush them in
//! depth-first order.
//!
//! A deep tree of `a/b/c/<10 files>`, `a/b/d/<10 files>`, `a/e/f/<10
//! files>` therefore produces five sub-list segments behind the initial
//! top-level segment (b, c, d, e, f) and exercises:
//!
//! - the segment iteration loop in `SegmentScheduler::next_if_needed`
//!   firing more than once per transfer,
//! - `parent_dir_ndx` alignment across multiple depth-first hops,
//! - the upstream receiver's `dir_flist` growth path
//!   (`flist.c:recv_file_list()` 2643-onward) accepting more than one
//!   sub-list header in sequence,
//! - and the final `NDX_FLIST_EOF` marker firing only after all five
//!   sub-segments have been consumed.
//!
//! With ten files per leaf directory the per-segment payload is well
//! below `MIN_FILECNT_LOOKAHEAD = 1000`, so the scheduler dispatches
//! every segment back-to-back without throttling. That is intentional:
//! we want to maximize the number of distinct `NDX_FLIST_OFFSET`
//! headers on the wire without depending on a particular timing
//! pattern.
//!
//! ## Platform gate
//!
//! `#[cfg(all(unix, not(target_os = "macos")))]` - the upstream rsync
//! binaries the harness depends on are only pre-built for Linux in
//! `tools/ci/run_interop.sh`. macOS has no `target/interop/upstream-install`
//! tree in standard CI, so the test would be a perpetual skip there.
//!
//! ## Upstream availability
//!
//! Looked up via `integration::helpers::upstream_rsync_binary("3.4.1")`.
//! If `target/interop/upstream-install/3.4.1/bin/rsync` is missing the
//! test logs `skip:` and returns successfully. Run
//! `bash tools/ci/run_interop.sh` to populate the install tree.
//!
//! ## Wire-marker assertion
//!
//! Per the task spec for ISI.d the deeper protocol parse (decoding
//! `MSG_DATA` frames from the sender's stdout to count `NDX_FLIST_OFFSET`
//! sentinels) is deferred to ISI.e. Asserting it here would require
//! teeing `server_stdout` through a multiplex-frame decoder and
//! walking each `NDX_*` token, which is a larger surface than the
//! surgical-changes rule allows for a single new test file.
//!
//! TODO: ISI.e asserts wire-byte parity (count `NDX_FLIST_OFFSET`
//! sentinels in the sender's outbound stream and confirm it equals
//! the number of sub-list segments the partitioner produced).

#![cfg(all(unix, not(target_os = "macos")))]

mod integration;

use integration::helpers::{TestDir, upstream_rsync_binary};

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use checksums::strong::Sha256;

/// Upstream rsync 3.4.1 protocol-32 capability string with `'i'` set.
///
/// Sender sends this when it advertises INC_RECURSE to the receiver.
/// Mirrors the sister harness in `inc_recurse_single_segment_push_isi_c.rs`.
const UPSTREAM_FLAGS_341: &str = "-vlogDtprze.iLsfxCIvu";

/// Layout descriptor: relative directory path, file count.
///
/// Three leaf directories nested under shared parents force the
/// classifier in `partition_file_list_for_inc_recurse` to emit one
/// `DirSegment` per non-root directory (five segments: `a/b`, `a/b/c`,
/// `a/b/d`, `a/e`, `a/e/f`). Total file count stays small to keep the
/// pipe-driven harness fast.
const TREE_LAYOUT: &[(&str, usize)] = &[("a/b/c", 10), ("a/b/d", 10), ("a/e/f", 10)];

/// Number of regular files in the deep-tree fixture.
const EXPECTED_FILE_COUNT: usize = 30;

/// Number of directories in the deep-tree fixture (`a`, `a/b`, `a/b/c`,
/// `a/b/d`, `a/e`, `a/e/f`). Exposed as a constant so the test asserts
/// the snapshot shape explicitly.
const EXPECTED_DIR_COUNT: usize = 6;

/// Locate the oc-rsync binary built with the current feature set.
///
/// Prefers the Cargo-provided `CARGO_BIN_EXE_oc-rsync` env var so the
/// binary used matches whatever feature flags the test run was launched
/// with. Falls back to walking up from the test executable, matching
/// the pattern in `inc_recurse_single_segment_push_isi_c.rs::locate_oc_rsync`.
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

/// Deterministic deep tree: three leaf directories, ten files each.
///
/// File sizes step through small, block-boundary, and odd values so a
/// later signature/delta assertion can be added without rewriting the
/// fixture. Content bytes encode both the file index and the directory
/// path so two same-size files in different directories still differ -
/// a regression smoke test for any classifier that confuses entries by
/// base name alone.
fn build_multi_segment_tree(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let sizes = [0usize, 1, 7, 64, 256, 1024, 4096, 4097, 8192, 12345];

    let mut total = 0usize;
    for (dir_idx, (rel_dir, count)) in TREE_LAYOUT.iter().enumerate() {
        assert_eq!(
            *count,
            sizes.len(),
            "TREE_LAYOUT entries must match the canonical size table"
        );
        let dir_path = root.join(rel_dir);
        fs::create_dir_all(&dir_path)?;

        for (file_idx, size) in sizes.iter().enumerate() {
            let name = format!("file_{file_idx:02}.bin");
            let mut buf = Vec::with_capacity(*size);
            for byte_idx in 0..*size {
                // Per-byte mix folds dir_idx, file_idx, and the byte
                // offset so each (dir, file, byte) tuple is unique.
                let mixed = (dir_idx as u32)
                    .wrapping_mul(1009)
                    .wrapping_add((file_idx as u32).wrapping_mul(31))
                    .wrapping_add(byte_idx as u32);
                buf.push(mixed as u8);
            }
            fs::write(dir_path.join(&name), &buf)?;
            total += 1;
        }
    }
    assert_eq!(
        total, EXPECTED_FILE_COUNT,
        "TREE_LAYOUT must total EXPECTED_FILE_COUNT files"
    );
    Ok(())
}

/// Map of relative path -> SHA-256 digest of file contents.
///
/// Walks the destination recursively (unlike the flat snapshot used by
/// ISI.c) since the deep-tree fixture has nested directories by design.
type Snapshot = BTreeMap<PathBuf, [u8; 32]>;

fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let mut out = Snapshot::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

fn walk(base: &Path, current: &Path, out: &mut Snapshot) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_dir() {
            walk(base, &path, out)?;
        } else if ft.is_file() {
            let rel = path.strip_prefix(base).unwrap().to_path_buf();
            let bytes = fs::read(&path)?;
            out.insert(rel, Sha256::digest(&bytes));
        } else {
            return Err(io::Error::other(format!(
                "multi-segment fixture is files-and-dirs only; saw {} ({:?})",
                path.display(),
                ft
            )));
        }
    }
    Ok(())
}

/// Count directories in the snapshotted tree.
///
/// Used as a structural sanity check: the receiver must materialize
/// every intermediate directory in the deep tree, not just the leaves
/// that hold files.
fn count_dirs(root: &Path) -> io::Result<usize> {
    fn recurse(dir: &Path, count: &mut usize) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.file_type().is_dir() {
                *count += 1;
                recurse(&path, count)?;
            }
        }
        Ok(())
    }
    let mut count = 0;
    recurse(root, &mut count)?;
    Ok(count)
}

fn diff_snapshots(src: &Snapshot, dst: &Snapshot) -> Vec<String> {
    let mut errors = Vec::new();
    for (p, sd) in src {
        match dst.get(p) {
            None => errors.push(format!("missing in dst: {}", p.display())),
            Some(dd) if dd != sd => errors.push(format!("digest mismatch: {}", p.display())),
            _ => {}
        }
    }
    for p in dst.keys() {
        if !src.contains_key(p) {
            errors.push(format!("extra in dst: {}", p.display()));
        }
    }
    errors
}

/// Pump bytes from `reader` to `writer` until EOF, flushing as we go.
///
/// Lifted verbatim from the ISI.c harness so the two tests share their
/// pipe semantics. The surgical-changes rule keeps this duplicated
/// rather than extracted to a shared helper until ISI.e or later
/// introduces a third caller that justifies a common module.
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

/// Drive the oc-rsync sender against an upstream rsync receiver via
/// wired stdio pipes.
///
/// Same shape as ISI.c's `run_pipe_push`. Reproduced inline (not
/// extracted) per the surgical-changes rule: a shared helper appears
/// when ISI.e adds a third pipe-driven caller, not before.
fn run_pipe_push(oc_bin: &Path, up_bin: &Path, src: &Path, dst: &Path) -> io::Result<()> {
    let mut server = Command::new(oc_bin)
        .arg("--server")
        .arg("--sender")
        .arg(UPSTREAM_FLAGS_341)
        .arg(".")
        .arg(src.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut client = Command::new(up_bin)
        .arg("--server")
        .arg(UPSTREAM_FLAGS_341)
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

    let s2c = thread::spawn(move || -> io::Result<()> {
        let mut reader = std::io::BufReader::new(server_stdout);
        let mut writer = std::io::BufWriter::new(client_stdin);
        copy_until_eof(&mut reader, &mut writer)
    });

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

/// End-to-end push: deep tree, multi-segment INC_RECURSE, byte-identical destination.
///
/// Skips with `skip:` (and succeeds) when the upstream binary is not
/// available, matching the convention used by every other interop test
/// that depends on `target/interop/upstream-install/`. Run
/// `bash tools/ci/run_interop.sh` to populate the tree.
#[test]
fn multi_segment_push_to_upstream_3_4_1_byte_identical() {
    let oc_bin = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            eprintln!("skip: oc-rsync binary not located");
            return;
        }
    };
    let up_bin = match upstream_rsync_binary("3.4.1") {
        Some(p) => p,
        None => {
            eprintln!(
                "skip: upstream rsync 3.4.1 not installed at \
                 target/interop/upstream-install/3.4.1/bin/rsync; \
                 run tools/ci/run_interop.sh"
            );
            return;
        }
    };

    let test_dir = TestDir::new().expect("create test dir");
    let src = test_dir.mkdir("src").expect("mkdir src");
    let dst = test_dir.mkdir("dst").expect("mkdir dst");

    build_multi_segment_tree(&src).expect("populate source tree");

    let src_snap = snapshot(&src).expect("snapshot source");
    assert_eq!(
        src_snap.len(),
        EXPECTED_FILE_COUNT,
        "multi-segment fixture must produce exactly {EXPECTED_FILE_COUNT} files"
    );
    assert_eq!(
        count_dirs(&src).expect("count source dirs"),
        EXPECTED_DIR_COUNT,
        "multi-segment fixture must produce exactly {EXPECTED_DIR_COUNT} directories"
    );

    run_pipe_push(&oc_bin, &up_bin, &src, &dst).expect("pipe push must succeed");

    let dst_snap = snapshot(&dst).expect("snapshot destination");
    let diffs = diff_snapshots(&src_snap, &dst_snap);
    assert!(
        diffs.is_empty(),
        "destination diverged from source after multi-segment INC_RECURSE push:\n{}",
        diffs.join("\n")
    );

    let dst_dirs = count_dirs(&dst).expect("count destination dirs");
    assert_eq!(
        dst_dirs, EXPECTED_DIR_COUNT,
        "destination must materialize every intermediate directory; \
         expected {EXPECTED_DIR_COUNT}, got {dst_dirs}"
    );
}
