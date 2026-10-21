//! Byte-neutrality of the lazy INC_RECURSE sender producer across the option
//! and transport matrix.
//!
//! The lazy producer (upstream `flist.c:2636 send_extra_file_list()` /
//! `send1extra()`) reads each directory only when its sub-list is due; the
//! eager producer reads the whole tree first. Both must put the same bytes on
//! the wire in both directions, for every option the lazy producer accepts,
//! with the oc sender on either end of a remote shell.
//!
//! Each cell runs the same transfer twice, `OC_RSYNC_LAZY_FLIST=0` and `=1`,
//! and requires identical transcripts and identical destination trees. The
//! fixture crosses `MIN_FILECNT_LOOKAHEAD` (1000 entries) so the sub-lists
//! interleave with file data the way a real transfer does.
//!
//! The comparison is only meaningful if the lazy arm really ran the lazy
//! producer, so a per-direction engagement cell proves that through the
//! sender's `--debug=flist3` trace. The trace goes to a separate run, because
//! debug output would itself move the wire.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::FileTime;
use test_support::WireTranscript;
use test_support::transcript::TranscriptRecorder;

/// Fixed mtime for every fixture entry (2023-11-14T22:13:20Z), so quick-check
/// and flist mtimes are identical across fixture builds.
const FIXED_MTIME_SECS: i64 = 1_700_000_000;

/// Sub-directories of the wide level, each one INC_RECURSE sub-list.
const WIDE_DIRS: usize = 12;
/// Files per wide sub-directory; `WIDE_DIRS * WIDE_FILES` exceeds the
/// 1000-entry `MIN_FILECNT_LOOKAHEAD`.
const WIDE_FILES: usize = 100;

/// The lazy producer's trace line (`inc_recurse.rs`), printed at FLIST3.
const LAZY_TRACE: &str = "inc_recurse producer: lazy";

const LAZY_ENV: &str = "OC_RSYNC_LAZY_FLIST";

#[derive(Clone, Copy, Debug)]
enum Direction {
    /// `oc-rsync SRC/ host:DEST` - the client is the sender.
    Push,
    /// `oc-rsync host:SRC/ DEST` - the server is the sender.
    Pull,
}

/// Builds a multi-level tree with distinct, non-periodic file contents, a
/// symlink, a per-directory merge file and files a filter cell excludes.
fn build_fixture(root: &Path) {
    let mut seed = 0x1a2b_u64;
    let mut write = |rel: &str, len: usize| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let data: Vec<u8> = (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            })
            .collect();
        fs::write(path, data).expect("write fixture file");
    };
    write("top.bin", 3000);
    write("deep/a/b/c/leaf.bin", 1500);
    write("deep/a/b/mid.skip", 64);
    write("deep/a/side.bin", 700);
    for d in 0..WIDE_DIRS {
        for f in 0..WIDE_FILES {
            write(&format!("wide/w{d:02}/f{f:03}"), 16 + f);
        }
    }
    fs::write(root.join("deep/a/.rsync-filter"), "- *.skip\n").expect("merge file");
    std::os::unix::fs::symlink("../top.bin", root.join("deep/up")).expect("symlink");
    backdate_tree(root);
}

/// Sets every entry (including directories and the symlink itself) to
/// [`FIXED_MTIME_SECS`], deepest first so a parent's mtime is not bumped.
fn backdate_tree(root: &Path) {
    let stamp = FileTime::from_unix_time(FIXED_MTIME_SECS, 0);
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            if fs::symlink_metadata(&path).expect("lstat").is_dir() {
                stack.push(path.clone());
            }
            entries.push(path);
        }
    }
    entries.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    entries.push(root.to_path_buf());
    for path in entries {
        filetime::set_symlink_file_times(&path, stamp, stamp).expect("backdate");
    }
}

/// Every entry under `root`, relative, with file content or symlink target.
fn tree_listing(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            let rel = path.strip_prefix(root).expect("under root").to_path_buf();
            let meta = fs::symlink_metadata(&path).expect("lstat");
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).expect("readlink");
                out.push((rel, target.into_os_string().into_encoded_bytes()));
            } else if meta.is_dir() {
                out.push((rel, Vec::new()));
                stack.push(path);
            } else {
                out.push((rel, fs::read(&path).expect("read file")));
            }
        }
    }
    out.sort();
    out
}

/// One captured transfer: its transcript, destination tree and the client's
/// combined stdout and stderr (debug lines are FINFO, which goes to stdout).
struct Run {
    transcript: WireTranscript,
    dest: Vec<(PathBuf, Vec<u8>)>,
    output: String,
}

/// Runs one transfer of `src` in `direction` with `opts`, the lazy flag set to
/// `lazy`, and the given server binary.
fn run(src: &Path, direction: Direction, opts: &[&str], lazy: bool, server: &Path) -> Run {
    let workdir = test_support::create_tempdir();
    let dest = workdir.path().join("dest");
    fs::create_dir(&dest).expect("mkdir dest");
    let recorder = TranscriptRecorder::new(workdir.path());
    let client = test_support::oc_rsync_bin();
    let capture_rsh = Path::new(env!("CARGO_BIN_EXE_capture-rsh"));
    let mut cmd: Command = recorder.command(&client, server, capture_rsh);
    cmd.args(opts).env(LAZY_ENV, if lazy { "1" } else { "0" });
    match direction {
        Direction::Push => cmd
            .arg(format!("{}/", src.display()))
            .arg(format!("transcripthost:{}", dest.display())),
        Direction::Pull => cmd
            .arg(format!("transcripthost:{}/", src.display()))
            .arg(&dest),
    };
    let output = cmd.output().expect("run oc-rsync client");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "{direction:?} {opts:?} lazy={lazy} failed: {}\n{text}",
        output.status
    );
    let mut transcript = recorder.finish().expect("read captures");
    if matches!(direction, Direction::Pull) {
        transcript
            .mask_pull_flist_times()
            .expect("mask pull flist times");
    }
    Run {
        transcript,
        dest: tree_listing(&dest),
        output: text,
    }
}

/// Requires the lazy run to be byte-identical to the eager one, both ways.
fn assert_lazy_matches_eager(direction: Direction, opts: &[&str]) {
    let src = test_support::create_tempdir();
    build_fixture(src.path());
    let server = test_support::oc_rsync_bin();
    let eager = run(src.path(), direction, opts, false, &server);
    let lazy = run(src.path(), direction, opts, true, &server);
    let what = format!("{direction:?} {opts:?}");
    assert!(
        eager.transcript.client_to_server == lazy.transcript.client_to_server,
        "{what}: client->server streams differ ({} vs {} bytes)",
        eager.transcript.client_to_server.len(),
        lazy.transcript.client_to_server.len()
    );
    assert!(
        eager.transcript.server_to_client == lazy.transcript.server_to_client,
        "{what}: server->client streams differ ({} vs {} bytes)",
        eager.transcript.server_to_client.len(),
        lazy.transcript.server_to_client.len()
    );
    assert_eq!(eager.dest, lazy.dest, "{what}: destination trees differ");
}

/// Option sets the lazy producer accepts (`lazy_producer_eligible`).
const OPTION_MATRIX: &[&[&str]] = &[
    &["-rlt"],
    &["-rlpt"],
    &["-rlt", "-z"],
    &["-rlt", "-c"],
    &["-rlt", "-F"],
    &["-rlt", "--exclude=*.skip"],
    &["-rlt", "--delete"],
    &["-rlt", "--numeric-ids"],
    &["-rlt", "-x"],
    &["-rlt", "--no-whole-file"],
];

#[test]
fn lazy_push_is_byte_identical_to_eager_across_options() {
    for opts in OPTION_MATRIX {
        assert_lazy_matches_eager(Direction::Push, opts);
    }
}

#[test]
fn lazy_pull_is_byte_identical_to_eager_across_options() {
    for opts in OPTION_MATRIX {
        assert_lazy_matches_eager(Direction::Pull, opts);
    }
}

/// A server wrapper that adds `--debug=flist3`, which the client never
/// forwards (upstream `options.c:3128` sends only `--info`).
fn debug_server(dir: &Path) -> PathBuf {
    let script = dir.join("oc-rsync-flist-debug");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nexec '{}' --debug=flist3 \"$@\"\n",
            test_support::oc_rsync_bin().display()
        ),
    )
    .expect("write server wrapper");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod wrapper");
    script
}

/// Non-vacuity: with the flag on, the sender in each direction runs the lazy
/// producer, and with it off it does not. Without this the matrix above could
/// compare two eager runs.
#[test]
fn the_lazy_flag_engages_the_lazy_producer_in_both_directions() {
    let src = test_support::create_tempdir();
    build_fixture(src.path());
    let wrappers = test_support::create_tempdir();
    let debug_server = debug_server(wrappers.path());
    let plain_server = test_support::oc_rsync_bin();
    for direction in [Direction::Push, Direction::Pull] {
        // A push sender is the client, so the flag goes on the client side;
        // a pull sender is the server, reached through the wrapper.
        let (opts, server): (&[&str], &Path) = match direction {
            Direction::Push => (&["-rlt", "--debug=flist3"], &plain_server),
            Direction::Pull => (&["-rlt"], &debug_server),
        };
        let lazy = run(src.path(), direction, opts, true, server);
        let eager = run(src.path(), direction, opts, false, server);
        assert!(
            lazy.output.contains(LAZY_TRACE),
            "{direction:?}: the flag must engage the lazy producer:\n{}",
            lazy.output
        );
        assert!(
            !eager.output.contains(LAZY_TRACE),
            "{direction:?}: the eager arm must not run the lazy producer:\n{}",
            eager.output
        );
    }
}
