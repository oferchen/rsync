//! Oracle cells: per-role debug trace lines of a local copy against rsync 3.5.2.
//!
//! Upstream forks the roles of a local copy, so lines from different roles
//! interleave by scheduling: two runs of 3.5.2 on the same tree can print them
//! in different orders. Each cell therefore compares one role's lines, in
//! order, rather than the whole transcript.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// A tree with a file, a symlink and a subdirectory, so the run crosses an
/// INC_RECURSE directory boundary.
fn fixture(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a"), b"hello\n").unwrap();
    fs::write(src.join("sub/b"), vec![0u8; 3000]).unwrap();
    std::os::unix::fs::symlink("a", src.join("l")).unwrap();
}

/// Stdout of a local `-a` copy of the fixture with the given debug words.
fn run(binary: &Path, debug: &str) -> String {
    run_with(binary, &[&format!("--debug={debug}")])
}

/// Stdout of a local `-a` copy of the fixture with extra options.
fn run_with(binary: &Path, options: &[&str]) -> String {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let output = Command::new(binary)
        .current_dir(dir.path())
        .arg("-a")
        .args(options)
        .args(["src/", "dst/"])
        .output()
        .expect("run rsync");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The lines starting with `prefix`, in the order printed.
fn role_lines(transcript: &str, prefix: &str) -> Vec<String> {
    transcript
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .collect()
}

/// upstream: receiver.c:1393-1394 prints `recv_files finished` as recv_files()
/// returns and generator.c:3020 prints `generate_files finished` as
/// generate_files() returns, both before the goodbye exchange. A server that
/// queued them after the goodbye let the client close first and lost them.
#[test]
fn receiver_and_generator_print_their_finished_lines() {
    let oc = run(&oc_rsync_bin(), "recv1,genr1");
    let receiver = role_lines(&oc, "recv_files");
    let generator = role_lines(&oc, "generate_files");
    assert_eq!(
        receiver.last().map(String::as_str),
        Some("recv_files finished"),
        "{oc}"
    );
    assert_eq!(
        generator.last().map(String::as_str),
        Some("generate_files finished"),
        "{oc}"
    );

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let reference = run(upstream.binary(), "recv1,genr1");
    assert_eq!(receiver.last(), role_lines(&reference, "recv_files").last());
    assert_eq!(generator, role_lines(&reference, "generate_files"));
}

/// The receiver role's lines, with the process id replaced by `N`.
fn receiver_lines(transcript: &str) -> Vec<String> {
    transcript
        .lines()
        .filter(|line| line.starts_with("server_recv(") || line.starts_with("recv_files"))
        .map(|line| match line.split_once(" pid=") {
            Some((head, _)) => format!("{head} pid=N"),
            None => line.to_owned(),
        })
        .collect()
}

/// upstream: main.c:1198-1199 - the forked receiver of a local copy runs
/// do_server_recv(), whose first trace line names its argument count (`.` and
/// the destination) and pid, before any `recv_files` line.
#[test]
fn server_receiver_announces_itself_first() {
    let oc = receiver_lines(&run(&oc_rsync_bin(), "recv1"));
    assert_eq!(
        oc.first().map(String::as_str),
        Some("server_recv(2) starting pid=N"),
        "{oc:?}"
    );

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let reference = receiver_lines(&run(upstream.binary(), "recv1"));
    assert_eq!(oc.first(), reference.first());
}

/// The receiver's `renaming` lines with each temp file's random six-character
/// suffix replaced by `XXXXXX`.
fn rename_lines(transcript: &str) -> Vec<String> {
    transcript
        .lines()
        .filter_map(|line| line.strip_prefix("renaming "))
        .map(|rest| {
            let (temp, dest) = rest.split_once(" to ").expect("renaming X to Y");
            let stem = temp.get(..temp.len().saturating_sub(6)).unwrap_or(temp);
            format!("renaming {stem}XXXXXX to {dest}")
        })
        .collect()
}

/// upstream: rsync.c:916-917 finish_transfer() names each temp file and its
/// destination, both relative to the destination directory, as it moves the
/// received file into place.
#[test]
fn receiver_names_each_temp_file_it_renames() {
    let oc = rename_lines(&run(&oc_rsync_bin(), "recv1"));
    assert_eq!(
        oc,
        ["renaming .a.XXXXXX to a", "renaming sub/.b.XXXXXX to sub/b"],
        "{oc:?}"
    );

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    assert_eq!(oc, rename_lines(&run(upstream.binary(), "recv1")));
}

/// The `landlock:` lines of a local `-a` copy into an existing destination
/// (the server receiver sandboxes only a destination that exists).
#[cfg(target_os = "linux")]
fn landlock_lines(options: &[&str]) -> Vec<String> {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    fs::create_dir(dir.path().join("dst")).unwrap();
    let output = Command::new(oc_rsync_bin())
        .current_dir(dir.path())
        .args(["-a"])
        .args(options)
        .args(["src/", "dst/"])
        .output()
        .expect("run oc-rsync");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&[output.stdout, output.stderr].concat())
        .lines()
        .filter(|line| line.starts_with("landlock:"))
        .map(str::to_owned)
        .collect()
}

/// rsync 3.5.2 prints no Landlock line at any verbosity, so oc's `-vvv` must
/// not either. The explicit oc-only word still reaches the server receiver,
/// which proves the copy took the sandboxed path the bare `-vvv` stays quiet on.
#[cfg(target_os = "linux")]
#[test]
fn verbosity_alone_prints_no_landlock_line() {
    for option in ["-vvv", "-vvvv"] {
        assert_eq!(landlock_lines(&[option]), Vec::<String>::new(), "{option}");
    }
    // Empty only on a kernel without Landlock, where the cell above is moot.
    let explicit = landlock_lines(&["-vvv", "--debug=landlock"]);
    assert!(
        explicit
            .iter()
            .all(|line| line.ends_with("enforced over 1 root(s)")),
        "{explicit:?}"
    );
}

/// The first word of each line that marks the receiver resolving the
/// destination or the generator starting, in the order printed.
fn startup_order(transcript: &str) -> Vec<&str> {
    transcript
        .lines()
        .filter(|line| {
            [
                "get_local_name ",
                "generator starting ",
                "delta-transmission ",
            ]
            .iter()
            .any(|head| line.starts_with(head))
        })
        .filter_map(|line| line.split(' ').next())
        .collect()
}

/// upstream: main.c:1248-1290 - the receiver reads the file list and resolves
/// the destination before do_recv() forks the generator, whose
/// generate_files() then prints its first two lines (generator.c:2836-2854).
#[test]
fn generator_starts_after_the_destination_is_resolved() {
    let oc = run(&oc_rsync_bin(), "recv1,genr1,flist1");
    assert_eq!(
        startup_order(&oc),
        ["get_local_name", "generator", "delta-transmission"],
        "{oc}"
    );

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let reference = run(upstream.binary(), "recv1,genr1,flist1");
    assert_eq!(startup_order(&oc), startup_order(&reference));
}

/// The `recv_files(<name>)` lines naming one entry each, in the order printed.
fn received_names(transcript: &str) -> Vec<&str> {
    transcript
        .lines()
        .filter(|line| line.starts_with("recv_files(") && line.ends_with(')'))
        .collect()
}

/// upstream: receiver.c:900-901 - the receiver names every index it reads,
/// including the itemize-only echoes of the directories and the symlink,
/// which the sender never transfers.
#[test]
fn receiver_names_every_entry_it_reads() {
    let flat = run_with(&oc_rsync_bin(), &["--debug=recv1", "--no-inc-recursive"]);
    let flat_names = received_names(&flat);
    for entry in ["recv_files(.)", "recv_files(sub)", "recv_files(l)"] {
        assert!(flat_names.contains(&entry), "{entry} missing: {flat}");
    }
    let incremental = run(&oc_rsync_bin(), "recv1");
    let names = received_names(&incremental);
    for entry in ["recv_files(.)", "recv_files(sub)", "recv_files(l)"] {
        assert!(names.contains(&entry), "{entry} missing: {incremental}");
    }

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let reference = run_with(upstream.binary(), &["--debug=recv1", "--no-inc-recursive"]);
    assert_eq!(flat_names, received_names(&reference));
    // Under incremental recursion each directory's row comes back on the gap
    // NDX below its sub-list, which names the directory (receiver.c:880-886).
    let reference = run(upstream.binary(), "recv1");
    assert_eq!(names, received_names(&reference));
}
