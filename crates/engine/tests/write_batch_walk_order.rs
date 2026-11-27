//! A local `--write-batch` walks its source in rsync 3.5.2's order and
//! patches a streamed record's flags in place.
//!
//! The batch is written as the walk goes, so the walk visits entries in the
//! order their records go out: each directory's files in `f_name_cmp` order,
//! then its subdirectories depth-first. That is also the order rsync 3.5.2's
//! generator itemizes them, so `-i` and `-v` rows of a batch-writing copy
//! match 3.5.2's row for row. The tree puts files after subdirectories at
//! several levels and uses names whose byte order differs from `f_name_cmp`
//! order (`a`, `a-b`, `a/`, `a.txt`).
//!
//! A record larger than the 1 MiB threshold goes to the batch before its
//! file is finished; its iflags word gains the size and time report bits only
//! after the body is out and is rewritten in place. rsync 3.5.2's
//! `--read-batch -i` prints those bits, so its rows for oc's batch must equal
//! its rows for its own batch of the same change.
//!
//! Both cells need the rsync 3.5.2 oracle (`OC_RSYNC_UPSTREAM_BIN_3_5_2`,
//! required under `OC_RSYNC_UPSTREAM_COMPAT=1`).
//!
//! # Upstream Reference
//!
//! - `generator.c:2259-2283` - the generator walks each list in NDX order
//! - `generator.c:2780-2788` - a directory is itemized at the gap NDX below
//!   its own sub-list
//! - `flist.c:f_name_cmp()` - files sort before directories in a list
//! - `log.c:log_item()` - a replayed record's iflags are printed as sent

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;
use test_support::{Deadlined, UpstreamVersion, run_deadlined};

const TIMEOUT: Duration = Duration::from_secs(120);

/// Files after subdirectories at three levels, with names whose byte order
/// and `f_name_cmp` order disagree.
fn build_walk_tree(root: &Path) {
    let files: &[&str] = &[
        "a-b",
        "a.txt",
        "zz",
        "a/a",
        "a/a.txt",
        "a/z",
        "a/m/x",
        "a/m/y",
        "a/m/deep/f",
        "b/q",
        "b/r",
        "b/a-b/s",
    ];
    for (i, name) in files.iter().enumerate() {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, format!("{name} {i}\n")).expect("write");
    }
    fs::create_dir_all(root.join("b/empty")).expect("mkdir");
}

fn output(cmd: &mut Command, what: &str) -> String {
    let Deadlined::Finished { status, stdout, stderr } =
        run_deadlined(cmd, TIMEOUT).unwrap_or_else(|e| panic!("spawn {what}: {e}"))
    else {
        panic!("{what} timed out after {TIMEOUT:?}");
    };
    assert!(status.success(), "{what} failed: {}", String::from_utf8_lossy(&stderr));
    String::from_utf8(stdout).expect("utf-8 output")
}

/// The rows of a run's stdout, without the byte-count trailer (task-tracked
/// separately: oc's sent/received totals differ from 3.5.2's).
fn rows(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter(|line| !line.starts_with("sent ") && !line.starts_with("total size is "))
        .filter(|line| !line.is_empty())
        .collect()
}

fn batch_copy_rows(binary: &Path, flag: &str, source: &Path, root: &Path, tag: &str) -> String {
    let mut src_arg = source.as_os_str().to_owned();
    src_arg.push("/");
    output(
        Command::new(binary)
            .arg("-r")
            .arg(flag)
            .arg(format!("--write-batch={}", root.join(format!("{tag}.batch")).display()))
            .arg(&src_arg)
            .arg(root.join(format!("{tag}.dst"))),
        &format!("{tag} --write-batch {flag}"),
    )
}

fn assert_rows_match_upstream(flag: &str) {
    let Some(oracle) = test_support::require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    build_walk_tree(&source);
    let ours = batch_copy_rows(&test_support::oc_rsync_bin(), flag, &source, temp.path(), "oc");
    let theirs = batch_copy_rows(oracle.binary(), flag, &source, temp.path(), "up");
    assert_eq!(rows(&ours), rows(&theirs), "{flag} rows of a batch-writing copy");
}

#[test]
fn itemize_rows_of_a_batch_writing_copy_follow_upstream_order() {
    assert_rows_match_upstream("-i");
}

#[test]
fn verbose_rows_of_a_batch_writing_copy_follow_upstream_order() {
    assert_rows_match_upstream("-v");
}

/// A destination holding an older, shorter `big` and nothing else: the copy
/// sends `big` as one literal record well past 1 MiB, and its row gains the
/// size and time bits only once the file is finished.
fn stage(root: &Path, name: &str) -> std::path::PathBuf {
    let dest = root.join(name);
    fs::create_dir(&dest).expect("mkdir");
    fs::write(dest.join("big"), vec![b'o'; 1 << 20]).expect("write");
    fs::File::options()
        .write(true)
        .open(dest.join("big"))
        .and_then(|file| {
            file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
        })
        .expect("mtime");
    dest
}

#[test]
fn a_record_past_the_threshold_replays_with_its_final_flags() {
    let Some(oracle) = test_support::require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let temp = tempdir().expect("tempdir");
    let root = temp.path();
    let source = root.join("src");
    fs::create_dir(&source).expect("mkdir");
    let body: Vec<u8> = (0..3u32 << 20).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    fs::write(source.join("big"), &body).expect("write");
    fs::write(source.join("small"), b"small\n").expect("write");
    let mut src_arg = source.as_os_str().to_owned();
    src_arg.push("/");

    let mut replays = Vec::new();
    for (tag, writer) in [("oc", test_support::oc_rsync_bin()), ("up", oracle.binary().to_path_buf())] {
        let batch = root.join(format!("{tag}.batch"));
        output(
            Command::new(&writer)
                .arg("-rt")
                .arg(format!("--write-batch={}", batch.display()))
                .arg(&src_arg)
                .arg(stage(root, &format!("{tag}.written"))),
            &format!("{tag} --write-batch"),
        );
        let replay = stage(root, &format!("{tag}.replay"));
        let stdout = output(
            Command::new(oracle.binary())
                .arg("-rt")
                .arg("-i")
                .arg(format!("--read-batch={}", batch.display()))
                .arg(&replay),
            &format!("rsync 3.5.2 --read-batch of the {tag} batch"),
        );
        assert_eq!(fs::read(replay.join("big")).expect("read"), body, "{tag} batch replays big");
        replays.push(stdout);
    }
    assert_eq!(
        rows(&replays[0]),
        rows(&replays[1]),
        "rsync 3.5.2 replays oc's batch with the flags of its own"
    );
    assert!(
        rows(&replays[0]).iter().any(|row| row.starts_with(">f.st") && row.ends_with(" big")),
        "the size and time bits reach the replay: {:?}",
        rows(&replays[0])
    );
}

/// The lines and exit code of a run, stderr apart from stdout: rsync 3.5.2's
/// sender reports from its own process, so only each stream's order is
/// fixed.
fn run_streams(cmd: &mut Command, what: &str) -> (Option<i32>, Vec<String>, Vec<String>) {
    let Deadlined::Finished { status, stdout, stderr } =
        run_deadlined(cmd, TIMEOUT).unwrap_or_else(|e| panic!("spawn {what}: {e}"))
    else {
        panic!("{what} timed out after {TIMEOUT:?}");
    };
    let lines = |bytes: Vec<u8>| -> Vec<String> {
        String::from_utf8(bytes)
            .expect("utf-8 output")
            .lines()
            .map(str::to_owned)
            .collect()
    };
    (status.code(), lines(stdout), lines(stderr))
}

/// An error trailer without the source location and role, which name each
/// implementation's own file and version.
fn without_location(line: &str) -> &str {
    if line.starts_with("rsync error: ") {
        line.split(" at ").next().unwrap_or(line)
    } else {
        line
    }
}

#[test]
fn unreadable_directories_report_like_upstream_when_listed_ahead() {
    use std::os::unix::fs::PermissionsExt;

    let Some(oracle) = test_support::require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    build_walk_tree(&source);
    // A large file listed before the subdirectories, so they are listed
    // ahead of its data.
    fs::write(source.join("big"), vec![7u8; 3 << 20]).expect("write");
    let locked = [source.join("b/locked"), source.join("a/m/locked")];
    for dir in &locked {
        fs::create_dir_all(dir).expect("mkdir");
        fs::write(dir.join("hidden"), b"hidden\n").expect("write");
        fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).expect("chmod");
    }
    // A reader that can open them anyway (root) cannot see the failure.
    let readable = fs::read_dir(&locked[0]).is_ok();
    let results: Vec<_> = if readable {
        Vec::new()
    } else {
        [("oc", test_support::oc_rsync_bin()), ("up", oracle.binary().to_path_buf())]
            .into_iter()
            .map(|(tag, binary)| {
                let mut src_arg = source.as_os_str().to_owned();
                src_arg.push("/");
                run_streams(
                    Command::new(&binary)
                        .arg("-rv")
                        .arg(format!("--write-batch={}", temp.path().join(format!("{tag}.batch")).display()))
                        .arg(&src_arg)
                        .arg(temp.path().join(format!("{tag}.dst"))),
                    &format!("{tag} --write-batch"),
                )
            })
            .collect()
    };
    for dir in &locked {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("chmod back");
    }
    let [(oc_code, oc_out, oc_err), (up_code, up_out, up_err)] = results.as_slice() else {
        return;
    };
    assert_eq!(oc_code, up_code, "exit code");
    assert_eq!(
        oc_err.iter().map(|l| without_location(l)).collect::<Vec<_>>(),
        up_err.iter().map(|l| without_location(l)).collect::<Vec<_>>(),
        "error lines and their order"
    );
    assert_eq!(rows(&oc_out.join("\n")), rows(&up_out.join("\n")), "-v rows");
}
