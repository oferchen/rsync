//! A tree deeper than upstream's confined resolver can walk transfers only
//! the levels 3.5.1 reaches, and fails the rest the way 3.5.1 does.
//!
//! upstream: syscall.c:2940 `DS_MAXDEPTH 1024` and syscall.c:3004-3010
//! `ds_push()` - a walk holding 1024 descriptors fails with `ENOMEM`. The
//! sender's `opendir` of a directory 1024 levels below its base fails there
//! (flist.c:2356), so the 1024th level's file is never listed. With `-L` the
//! sender lists everything and the receiver's walk refuses instead: the stat
//! of a file below 1024 parents (generator.c:2213-2223) and the mkdir of a
//! directory below 1024 parents (generator.c:1873-1887). Either way 1023 files
//! arrive and the run exits 23.
//!
//! Linux only: a 1024-level absolute path is past macOS's 1024-byte
//! `PATH_MAX`, where upstream stops at `MAXPATHLEN` long before this ceiling.
#![cfg(target_os = "linux")]
use std::fs;
use std::path::{Path, PathBuf};
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// The `ENOMEM` text the refusals carry, rendered by the formatter the product
/// uses from this platform's C library: upstream prints `strerror()`, which is
/// "Cannot allocate memory" on glibc and "Out of memory" on musl.
fn enomem_text() -> String {
    engine::local_copy::upstream_io_error(&std::io::Error::from_raw_os_error(libc::ENOMEM))
}

/// Builds `src/d/d/.../d` `levels` deep with a file `f` in every level, so
/// the file at level `n` lies below `n` directories.
fn deep_chain(root: &Path, levels: usize) -> PathBuf {
    let src = root.join("src");
    let mut dir = src.clone();
    for level in 1..=levels {
        dir.push("d");
        fs::create_dir_all(&dir).expect("mkdir level");
        fs::write(dir.join("f"), level.to_string()).expect("write level file");
    }
    src
}

/// Counts the files named `f` under `dir`, at any depth.
fn files_under(dir: &Path) -> usize {
    let mut count = 0;
    let mut next = Some(dir.to_path_buf());
    while let Some(current) = next.take() {
        let Ok(entries) = fs::read_dir(&current) else {
            break;
        };
        for entry in entries {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                next = Some(path);
            } else {
                count += 1;
            }
        }
    }
    count
}

#[derive(Clone, Copy)]
enum Direction {
    Pull,
    Push,
}

fn transfer(direction: Direction, src: &Path, dest: &Path, extra: &[&str]) -> CliOutput {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let (from, to) = match direction {
        Direction::Pull => (
            format!("localhost:{}/", src.display()),
            format!("{}/", dest.display()),
        ),
        Direction::Push => (
            format!("{}/", src.display()),
            format!("localhost:{}/", dest.display()),
        ),
    };
    OcRsyncCliRunner::new()
        .arg("-r")
        .args(extra)
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(from)
        .arg(to)
        .run()
        .expect("transfer run")
}

fn count_lines(out: &CliOutput, needle: &str) -> usize {
    out.stderr_str()
        .lines()
        .filter(|l| l.contains(needle))
        .count()
}

/// 1023 levels is the deepest tree upstream copies whole.
fn copies_the_deepest_walkable_tree(direction: Direction) {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let src = deep_chain(tmp.path(), 1023);
    let dest = tmp.path().join("dest");
    let out = transfer(direction, &src, &dest, &[]);
    out.assert_exit(0);
    assert_eq!(files_under(&dest), 1023, "stderr:\n{}", out.stderr_str());
}

/// One level more and the sender's opendir of the 1024th level is refused:
/// that level's file is missing and the run is partial.
fn stops_the_sender_at_the_1024th_level(direction: Direction) {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let src = deep_chain(tmp.path(), 1024);
    let dest = tmp.path().join("dest");
    let out = transfer(direction, &src, &dest, &[]);
    out.assert_exit(23);
    assert_eq!(files_under(&dest), 1023, "stderr:\n{}", out.stderr_str());
    assert_eq!(
        count_lines(&out, "rsync: [sender] opendir \""),
        1,
        "one refused opendir, stderr:\n{}",
        out.stderr_str()
    );
    assert!(
        out.stderr_contains(&format!("/d\" failed: {}", enomem_text())),
        "upstream's ENOMEM wording, stderr:\n{}",
        out.stderr_str()
    );
}

/// Under `-L` the sender lists every level and the receiver's walk refuses:
/// the file below 1024 parents fails its stat, the directory below 1024
/// parents fails its mkdir, and nothing beneath that directory is attempted.
fn stops_the_receiver_below_1024_parents(direction: Direction) {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let src = deep_chain(tmp.path(), 1026);
    let dest = tmp.path().join("dest");
    let out = transfer(direction, &src, &dest, &["-L"]);
    out.assert_exit(23);
    assert_eq!(files_under(&dest), 1023, "stderr:\n{}", out.stderr_str());
    for (needle, why) in [
        (
            "rsync: [generator] recv_generator: failed to stat \"",
            "the file below 1024 parents",
        ),
        (
            "rsync: [generator] recv_generator: mkdir \"",
            "the directory below 1024 parents",
        ),
        (
            "*** Skipping any contents from this failed directory ***",
            "the skipped subtree",
        ),
    ] {
        assert_eq!(
            count_lines(&out, needle),
            1,
            "exactly one line for {why}, stderr:\n{}",
            out.stderr_str()
        );
    }
    assert_eq!(
        count_lines(&out, &enomem_text()),
        2,
        "both refusals carry upstream's ENOMEM wording, stderr:\n{}",
        out.stderr_str()
    );
}

#[test]
fn pull_copies_a_1023_level_tree_whole() {
    copies_the_deepest_walkable_tree(Direction::Pull);
}

#[test]
fn push_copies_a_1023_level_tree_whole() {
    copies_the_deepest_walkable_tree(Direction::Push);
}

#[test]
fn pull_sender_refuses_the_1024th_level() {
    stops_the_sender_at_the_1024th_level(Direction::Pull);
}

#[test]
fn push_sender_refuses_the_1024th_level() {
    stops_the_sender_at_the_1024th_level(Direction::Push);
}

#[test]
fn pull_receiver_refuses_below_1024_parents_under_copy_links() {
    stops_the_receiver_below_1024_parents(Direction::Pull);
}

#[test]
fn push_receiver_refuses_below_1024_parents_under_copy_links() {
    stops_the_receiver_below_1024_parents(Direction::Push);
}
