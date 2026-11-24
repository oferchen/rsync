//! `--read-batch` replay honours the replaying generator's choices.
//!
//! Upstream runs the real generator locally during a replay (main.c:650-660):
//! it applies the quick check and the command-line skip options to the
//! destination as it is now, and writes the index of every file it still
//! wants into a pipe. The receiver reads that pipe for each recorded transfer
//! row (receiver.c:772-803 `gen_wants_ndx`); a row the generator did not ask
//! for is announced with `(Skipping batched update for "NAME")`, its recorded
//! data is discarded and the destination file is left alone
//! (receiver.c:980-993).
//!
//! Every expectation below was measured against rsync 3.5.2 on the same
//! fixture; `tests/batch_replay_upstream_parity.rs` holds the oracle cells.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use filetime::FileTime;

const SRC_MTIME: i64 = 1_600_000_000;

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// A source tree, the batch recorded from it into an empty directory, and a
/// scratch root for replay destinations.
struct Fixture {
    root: tempfile::TempDir,
    batch: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::recorded_with(&[])
    }

    /// Records the batch with `-a` plus `record_flags`.
    fn recorded_with(record_flags: &[&str]) -> Self {
        let root = tempfile::TempDir::new().expect("temp dir");
        let src = root.path().join("src");
        fs::create_dir_all(&src).expect("create src");
        fs::write(src.join("a"), b"aaaa\n").expect("write a");
        fs::write(src.join("b"), b"bbbbbbbb\n").expect("write b");
        fs::write(src.join("big"), vec![b'z'; 100_000]).expect("write big");
        for name in ["a", "b", "big"] {
            filetime::set_file_mtime(src.join(name), FileTime::from_unix_time(SRC_MTIME, 0))
                .expect("stamp source");
        }
        let batch = root.path().join("batch");
        let recorded = root.path().join("recorded");
        let batch_flag = format!("--write-batch={}", batch.display());
        let src_arg = format!("{}/", src.display());
        let recorded_arg = format!("{}/", recorded.display());
        let mut args = vec!["-a"];
        args.extend_from_slice(record_flags);
        args.extend([batch_flag.as_str(), &src_arg, &recorded_arg]);
        let out = run(&args);
        assert!(
            out.status.success(),
            "recording the batch must succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Self { root, batch }
    }

    fn src(&self) -> PathBuf {
        self.root.path().join("src")
    }

    /// A fresh, empty replay destination.
    fn dest(&self, name: &str) -> PathBuf {
        let dest = self.root.path().join(name);
        fs::create_dir_all(&dest).expect("create dest");
        dest
    }

    /// A replay destination that already holds every source file, unchanged.
    fn up_to_date_dest(&self, name: &str) -> PathBuf {
        let dest = self.dest(name);
        for file in ["a", "b", "big"] {
            fs::copy(self.src().join(file), dest.join(file)).expect("copy");
            filetime::set_file_mtime(dest.join(file), FileTime::from_unix_time(SRC_MTIME, 0))
                .expect("stamp dest");
        }
        dest
    }

    fn replay(&self, dest: &Path, flags: &[&str]) -> Output {
        let mut args = vec!["-a"];
        args.extend_from_slice(flags);
        let batch_flag = format!("--read-batch={}", self.batch.display());
        args.push(&batch_flag);
        let dest_arg = format!("{}/", dest.display());
        args.push(&dest_arg);
        run(&args)
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(oc_rsync())
        .args(args)
        .output()
        .expect("spawn oc-rsync")
}

fn stdout_lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn skip_line(name: &str) -> String {
    format!("(Skipping batched update for \"{name}\")")
}

fn assert_success(out: &Output, cell: &str) {
    assert!(
        out.status.success(),
        "{cell}: replay must exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An identical destination is up to date, so the generator wants nothing:
/// every recorded update is skipped and no file is rewritten (rsync 3.5.2
/// keeps each inode).
#[test]
fn replay_onto_an_up_to_date_destination_rewrites_nothing() {
    let fx = Fixture::new();
    let dest = fx.up_to_date_dest("uptodate");
    let inodes: Vec<u64> = ["a", "b", "big"]
        .iter()
        .map(|n| fs::metadata(dest.join(n)).expect("stat").ino())
        .collect();

    let out = fx.replay(&dest, &[]);

    assert_success(&out, "up-to-date");
    for (name, inode) in ["a", "b", "big"].iter().zip(inodes) {
        assert_eq!(
            fs::metadata(dest.join(name)).expect("stat").ino(),
            inode,
            "{name} must not be replaced by an up-to-date replay"
        );
    }
    assert_eq!(
        stdout_lines(&out),
        vec![skip_line("a"), skip_line("b"), skip_line("big")]
    );
}

/// A batch recorded without incremental recursion replays through the
/// single-list generator pass, whose requests decide the same way: the
/// missing file is written and the up-to-date ones are skipped.
#[test]
fn replay_of_a_batch_without_inc_recursion_applies_only_wanted_updates() {
    let fx = Fixture::recorded_with(&["--no-inc-recursive"]);
    let dest = fx.up_to_date_dest("noinc");
    fs::remove_file(dest.join("b")).expect("remove b");
    let inode = fs::metadata(dest.join("big")).expect("stat").ino();

    let out = fx.replay(&dest, &[]);

    assert_success(&out, "--no-inc-recursive");
    assert_eq!(fs::read(dest.join("b")).expect("read b"), b"bbbbbbbb\n");
    assert_eq!(fs::metadata(dest.join("big")).expect("stat").ino(), inode);
    assert_eq!(stdout_lines(&out), vec![skip_line("a"), skip_line("big")]);
}

/// rsync 3.5.2 counts a skipped batched update as transferred: the
/// `xferred_files` and `total_transferred_size` increments run before the
/// skip (receiver.c:975-976). An up-to-date replay of three files therefore
/// reports three transferred files totalling their recorded size.
#[test]
fn replay_counts_skipped_updates_as_transferred() {
    let fx = Fixture::new();
    let dest = fx.up_to_date_dest("stats");

    let out = fx.replay(&dest, &["--stats"]);

    assert_success(&out, "--stats");
    let lines = stdout_lines(&out);
    for expected in [
        "Number of regular files transferred: 3",
        "Total transferred file size: 100,014 bytes",
    ] {
        assert!(
            lines.iter().any(|line| line == expected),
            "missing {expected:?} in {lines:?}"
        );
    }
}

/// Under `-u` a destination file newer than the recorded one is kept.
#[test]
fn replay_with_update_keeps_a_newer_destination_file() {
    let fx = Fixture::new();
    let dest = fx.dest("newer");
    fs::write(dest.join("a"), b"NEWERdst\n").expect("seed newer");
    filetime::set_file_mtime(
        dest.join("a"),
        FileTime::from_unix_time(SRC_MTIME + 86_400, 0),
    )
    .expect("stamp newer");

    let out = fx.replay(&dest, &["-u"]);

    assert_success(&out, "-u");
    assert_eq!(fs::read(dest.join("a")).expect("read a"), b"NEWERdst\n");
    assert_eq!(fs::read(dest.join("b")).expect("read b"), b"bbbbbbbb\n");
    assert_eq!(stdout_lines(&out), vec![skip_line("a")]);
}

/// Under `--ignore-existing` an existing destination file is kept, whatever
/// its age.
#[test]
fn replay_with_ignore_existing_keeps_an_existing_file() {
    let fx = Fixture::new();
    let dest = fx.dest("existing");
    fs::write(dest.join("a"), b"OLDdst\n").expect("seed existing");
    filetime::set_file_mtime(
        dest.join("a"),
        FileTime::from_unix_time(SRC_MTIME - 86_400, 0),
    )
    .expect("stamp old");

    let out = fx.replay(&dest, &["--ignore-existing"]);

    assert_success(&out, "--ignore-existing");
    assert_eq!(fs::read(dest.join("a")).expect("read a"), b"OLDdst\n");
    assert_eq!(fs::read(dest.join("b")).expect("read b"), b"bbbbbbbb\n");
    assert_eq!(stdout_lines(&out), vec![skip_line("a")]);
}

/// `--max-size` on the replay skips a recorded file that is too large.
#[test]
fn replay_with_max_size_skips_a_too_large_file() {
    let fx = Fixture::new();
    let dest = fx.dest("maxsize");

    let out = fx.replay(&dest, &["--max-size=1k"]);

    assert_success(&out, "--max-size");
    assert!(!dest.join("big").exists(), "big must not be written");
    assert_eq!(fs::read(dest.join("a")).expect("read a"), b"aaaa\n");
    assert_eq!(stdout_lines(&out), vec![skip_line("big")]);
}

/// A replay trusts the recorded file list (options.c:2730-2731 sets
/// `trust_sender_filter` under `read_batch`), so a receiver-side exclude
/// neither aborts the replay nor stops the recorded file: rsync 3.5.2 exits 0
/// and writes it.
#[test]
fn replay_does_not_recheck_the_recorded_list_against_excludes() {
    let fx = Fixture::new();
    let dest = fx.dest("exclude");

    let out = fx.replay(&dest, &["--exclude=a"]);

    assert_success(&out, "--exclude");
    assert!(stdout_lines(&out).is_empty(), "{:?}", stdout_lines(&out));
    assert_eq!(fs::read(dest.join("a")).expect("read a"), b"aaaa\n");
}

/// A hard-link group is decided as a whole: the batch carries the group's
/// data under one member (the last, as rsync 3.5.2 records it), and a replay
/// into an empty destination must still create every member, linked.
#[test]
fn replay_with_hard_links_applies_the_recorded_group() {
    let root = tempfile::TempDir::new().expect("temp dir");
    let src = root.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("a"), b"linked\n").expect("write a");
    fs::hard_link(src.join("a"), src.join("b")).expect("link b");
    let batch = root.path().join("batch");
    let recorded = root.path().join("recorded");
    let out = run(&[
        "-aH",
        &format!("--write-batch={}", batch.display()),
        &format!("{}/", src.display()),
        &format!("{}/", recorded.display()),
    ]);
    assert_success(&out, "-H record");
    let dest = root.path().join("dest");
    fs::create_dir_all(&dest).expect("create dest");

    let out = run(&[
        "-aH",
        &format!("--read-batch={}", batch.display()),
        &format!("{}/", dest.display()),
    ]);

    assert_success(&out, "-H replay");
    assert_eq!(fs::read(dest.join("a")).expect("read a"), b"linked\n");
    assert_eq!(
        fs::metadata(dest.join("a")).expect("stat a").ino(),
        fs::metadata(dest.join("b")).expect("stat b").ino(),
        "a and b must be one file"
    );
    assert!(stdout_lines(&out).is_empty(), "{:?}", stdout_lines(&out));
}

const LARGE_LEN: usize = 100_000;

/// A compressed (`-z`, always zlib in a batch: compat.c:415-416) delta batch
/// whose first file, `large.dat`, carries block matches and a literal, and
/// whose second file, `z.txt`, must be transferred. The replay destination
/// holds `large.dat` at the recorded size and mtime, so the replaying
/// generator skips that row, while `z.txt` differs in size and is wanted.
struct CompressedDeltaFixture {
    root: tempfile::TempDir,
    batch: PathBuf,
}

impl CompressedDeltaFixture {
    fn recorded_by(recorder: &Path) -> Self {
        let root = tempfile::TempDir::new().expect("temp dir");
        let src = root.path().join("src");
        let recorded = root.path().join("recorded");
        fs::create_dir_all(&src).expect("create src");
        fs::create_dir_all(&recorded).expect("create recorded");
        let mut large = vec![b'A'; LARGE_LEN];
        large[50_000..50_021].copy_from_slice(b"MODIFIED_SECTION_HERE");
        fs::write(src.join("large.dat"), &large).expect("write large");
        fs::write(src.join("z.txt"), b"new contents of z\n").expect("write z");
        fs::write(recorded.join("large.dat"), vec![b'A'; LARGE_LEN]).expect("seed large");
        fs::write(recorded.join("z.txt"), b"old z\n").expect("seed z");
        for name in ["large.dat", "z.txt"] {
            filetime::set_file_mtime(src.join(name), FileTime::from_unix_time(SRC_MTIME, 0))
                .expect("stamp source");
        }
        let batch = root.path().join("batch");
        let out = Command::new(recorder)
            .args([
                "-a",
                "-I",
                "-z",
                "--no-whole-file",
                "--compress-choice=zlib",
            ])
            .arg(format!("--write-batch={}", batch.display()))
            .arg(format!("{}/", src.display()))
            .arg(format!("{}/", recorded.display()))
            .output()
            .expect("spawn recorder");
        assert_success(&out, "compressed delta record");
        Self { root, batch }
    }

    /// A destination where `large.dat` passes the quick check (recorded size
    /// and mtime, old contents) and `z.txt` does not.
    fn dest(&self, name: &str) -> PathBuf {
        let dest = self.root.path().join(name);
        fs::create_dir_all(&dest).expect("create dest");
        fs::write(dest.join("large.dat"), vec![b'A'; LARGE_LEN]).expect("seed large");
        filetime::set_file_mtime(
            dest.join("large.dat"),
            FileTime::from_unix_time(SRC_MTIME, 0),
        )
        .expect("stamp large");
        fs::write(dest.join("z.txt"), b"old z\n").expect("seed z");
        dest
    }

    fn replay(&self, replayer: &Path, dest: &Path) -> Output {
        Command::new(replayer)
            .arg("-a")
            .arg(format!("--read-batch={}", self.batch.display()))
            .arg(format!("{}/", dest.display()))
            .output()
            .expect("spawn replayer")
    }
}

/// Asserts oc completed the replay: `large.dat` skipped and untouched,
/// `z.txt` written from the batch.
fn assert_skip_then_transfer(out: &Output, dest: &Path, large_inode: u64, cell: &str) {
    assert_success(out, cell);
    assert!(
        stdout_lines(out).contains(&skip_line("large.dat")),
        "{cell}: {:?}",
        stdout_lines(out)
    );
    assert_eq!(
        fs::metadata(dest.join("large.dat")).expect("stat").ino(),
        large_inode
    );
    assert_eq!(
        fs::read(dest.join("z.txt")).expect("read z"),
        b"new contents of z\n"
    );
}

/// Deliberate deviation from rsync 3.5.2. Skipping a zlib delta row that has
/// block matches leaves 3.5.2's inflate history short of the matched data
/// (its discard has no basis, receiver.c:633-636/696-699), so the row's
/// literal fails with Z_DATA_ERROR and the whole replay aborts
/// (token.c:664-668). oc keeps the history the right length, completes the
/// replay and writes the next file.
#[test]
fn compressed_replay_continues_past_a_skipped_delta_with_matches() {
    let fx = CompressedDeltaFixture::recorded_by(&oc_rsync());
    let dest = fx.dest("dest");
    let inode = fs::metadata(dest.join("large.dat")).expect("stat").ino();

    let out = fx.replay(&oc_rsync(), &dest);

    assert_skip_then_transfer(&out, &dest, inode, "oc replay");
}

/// Oracle row pinning the deviation above on purpose: rsync 3.5.2 cannot
/// replay its own batch of this fixture (exit 12, "inflate returned -3"),
/// while oc replays the same 3.5.2 batch to completion.
#[test]
fn rsync_3_5_2_aborts_where_oc_completes_a_compressed_skip() {
    let Some(upstream) =
        test_support::require_upstream_rsync(test_support::UpstreamVersion::V3_5_2)
    else {
        return;
    };
    let fx = CompressedDeltaFixture::recorded_by(upstream.binary());

    let up_dest = fx.dest("upstream");
    let up = fx.replay(upstream.binary(), &up_dest);
    assert_eq!(
        up.status.code(),
        Some(12),
        "3.5.2 must abort as recorded; stderr: {}",
        String::from_utf8_lossy(&up.stderr)
    );
    assert!(
        String::from_utf8_lossy(&up.stderr).contains("inflate returned -3"),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );

    let oc_dest = fx.dest("oc");
    let inode = fs::metadata(oc_dest.join("large.dat")).expect("stat").ino();
    let oc = fx.replay(&oc_rsync(), &oc_dest);
    assert_skip_then_transfer(&oc, &oc_dest, inode, "oc replay of the 3.5.2 batch");
}
