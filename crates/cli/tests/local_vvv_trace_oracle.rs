//! Oracle: a local `-vvv` copy prints upstream rsync 3.5.1's per-role trace.
//!
//! upstream runs a local copy as three processes - the client sender and a
//! forked receiver and generator (main.c:1286 `local_child()`) - and their
//! debug lines reach stdout through one multiplexed stream, so the global
//! interleave varies from run to run (30 runs of one fixture gave 5 distinct
//! orders). What every run shares is each role's own sequence: a process
//! writes its lines in program order. The oracle therefore compares the
//! per-role projections of both outputs, plus the multiset of the file-list
//! receipt lines, which the receiver and the generator both print for every
//! INC_RECURSE sub-list (flist.c:2687 `recv_file_list()`).
//!
//! # Gating
//!
//! Self-skips unless `OC_RSYNC_UPSTREAM_COMPAT=1` and an upstream 3.5.1
//! binary resolves (override with `OC_RSYNC_UPSTREAM_BIN_3_5_1`). Once both
//! hold the comparison must pass: any difference is a parity defect.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use test_support::{
    UpstreamVersion, oc_rsync_bin, require_upstream_rsync, upstream_compat_enabled,
};

/// The process a trace line comes from, per its upstream emitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    /// The client sender (main.c `client_run()` -> sender.c, flist.c send side).
    Sender,
    /// The forked receiver (main.c `do_recv()` -> receiver.c).
    Receiver,
    /// The receiver's generator child (generator.c).
    Generator,
    /// File-list receipt lines that both the receiver and the generator print.
    FileListReceipt,
    /// The client's session banner and closing statistics.
    Session,
}

/// Line prefixes that identify each role's emitter.
const ROLE_PREFIXES: &[(&str, Role)] = &[
    // flist.c:2687-2740 recv_file_list() / recv_file_name() trace.
    ("recv_file_name(", Role::FileListReceipt),
    ("received ", Role::FileListReceipt),
    ("recv_file_list done", Role::FileListReceipt),
    // main.c / flist.c / sender.c / match.c on the sending side.
    ("[sender] ", Role::Sender),
    ("send_file_list done", Role::Sender),
    ("send_files", Role::Sender),
    ("send files finished", Role::Sender),
    ("calling match_sums", Role::Sender),
    ("sending file_sum", Role::Sender),
    ("false_alarms=", Role::Sender),
    ("sender finished", Role::Sender),
    ("built hash table", Role::Sender),
    ("hash search", Role::Sender),
    ("done hash search", Role::Sender),
    ("total: matches=", Role::Sender),
    // main.c do_server_recv() / get_local_name(), receiver.c.
    ("[receiver] ", Role::Receiver),
    ("server_recv(", Role::Receiver),
    ("get_local_name", Role::Receiver),
    ("created directory", Role::Receiver),
    ("recv_files", Role::Receiver),
    ("recv mapped", Role::Receiver),
    ("got file_sum", Role::Receiver),
    ("renaming ", Role::Receiver),
    // generator.c.
    ("[generator] ", Role::Generator),
    ("generator starting", Role::Generator),
    ("delta-transmission", Role::Generator),
    ("recv_generator(", Role::Generator),
    ("generating and sending sums", Role::Generator),
    ("count=", Role::Generator),
    ("generate_files", Role::Generator),
    // main.c output_summary() and the flist.c start banner.
    ("sending incremental file list", Role::Session),
    ("sent ", Role::Session),
    ("total size is", Role::Session),
];

/// Classifies one normalized line. A line no prefix claims is a `-v`
/// listing row, which the sender logs (sender.c:775 `log_item()`), unless it
/// is a generator `is uptodate` notice (generator.c:1771).
fn role_of(line: &str) -> Role {
    if line.ends_with(" is uptodate") {
        return Role::Generator;
    }
    ROLE_PREFIXES
        .iter()
        .find(|(prefix, _)| line.starts_with(prefix))
        .map_or(Role::Sender, |&(_, role)| role)
}

/// Replaces the run-specific parts of a line: pids, mkstemp suffixes, the
/// fixture root and the closing statistics.
fn normalize(line: &str, root: &str) -> String {
    // main.c:1054 output_summary() - upstream's own sent/received totals vary
    // by a few bytes from run to run, and the rate and speedup follow them.
    if line.starts_with("sent ") && line.ends_with(" bytes/sec") {
        return "sent <N> bytes  received <N> bytes  <RATE> bytes/sec".to_owned();
    }
    if let Some((size, _speedup)) = line.split_once("  speedup is ") {
        return format!("{size}  speedup is <N>");
    }
    let line = line.replace(root, "<ROOT>");
    let mut words: Vec<String> = Vec::new();
    for word in line.split(' ') {
        let word = if let Some(pid) = word.strip_prefix("pid=") {
            if pid.chars().all(|c| c.is_ascii_digit()) {
                "pid=<PID>".to_owned()
            } else {
                word.to_owned()
            }
        } else {
            mask_temp_suffix(word)
        };
        words.push(word);
    }
    words.join(" ")
}

/// Masks the random suffix of a receiver temp name (`.name.XXXXXX`), as
/// receiver.c:837 `renaming %s to %s` prints it.
fn mask_temp_suffix(word: &str) -> String {
    let (dir, base) = word.rsplit_once('/').map_or(("", word), |(d, b)| (d, b));
    let Some(rest) = base.strip_prefix('.') else {
        return word.to_owned();
    };
    let Some((name, suffix)) = rest.rsplit_once('.') else {
        return word.to_owned();
    };
    if suffix.len() != 6 || !suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
        return word.to_owned();
    }
    let masked = format!(".{name}.XXXXXX");
    if dir.is_empty() {
        masked
    } else {
        format!("{dir}/{masked}")
    }
}

/// One output split into its per-role sequences.
#[derive(Debug, PartialEq, Eq)]
struct Projection {
    sequences: BTreeMap<Role, Vec<String>>,
    receipt_counts: BTreeMap<String, usize>,
}

fn project(output: &str, root: &str) -> Projection {
    let mut sequences: BTreeMap<Role, Vec<String>> = BTreeMap::new();
    let mut receipt_counts = BTreeMap::new();
    for raw in output.lines().filter(|line| !line.is_empty()) {
        let line = normalize(raw, root);
        match role_of(&line) {
            Role::FileListReceipt => *receipt_counts.entry(line).or_insert(0) += 1,
            role => sequences.entry(role).or_default().push(line),
        }
    }
    Projection {
        sequences,
        receipt_counts,
    }
}

/// Builds the fixture under `root/src` with fixed contents and mtimes.
fn build_fixture(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub")).expect("create fixture dirs");
    fs::write(src.join("aaa.txt"), b"alpha content\n").expect("write aaa.txt");
    fs::write(src.join("bbb.txt"), b"bravo content\n").expect("write bbb.txt");
    fs::write(src.join("sub/big.bin"), vec![b'x'; 70_000]).expect("write big.bin");
    std::os::unix::fs::symlink("aaa.txt", src.join("link")).expect("create symlink");
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    for entry in ["aaa.txt", "bbb.txt", "sub/big.bin", "sub", ""] {
        let file = fs::File::open(src.join(entry)).expect("open fixture entry");
        file.set_modified(stamp).expect("set fixture mtime");
    }
}

/// Changes `aaa.txt` without changing its size, so the next run deltas it.
fn modify_fixture(root: &Path) {
    let path = root.join("src/aaa.txt");
    fs::write(&path, b"alpha CHANGED\n").expect("rewrite aaa.txt");
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_923_200);
    let file = fs::File::open(&path).expect("open aaa.txt");
    file.set_modified(stamp).expect("set aaa.txt mtime");
}

fn run_trace(binary: &Path, root: &Path, args: &[&str]) -> String {
    let output = Command::new(binary)
        .current_dir(root)
        .args(args)
        .args(["src/", "dst/"])
        .output()
        .expect("spawn rsync");
    assert!(
        output.status.success(),
        "{} {args:?} failed: {}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// Runs both scenarios - a fresh copy, then a delta of one changed file -
/// with `binary` in its own fixture, returning their projections.
fn trace_scenarios(binary: &Path) -> Vec<Projection> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical tempdir");
    let root_text = root.to_str().expect("utf-8 tempdir").to_owned();
    build_fixture(&root);
    let fresh = run_trace(binary, &root, &["-avvv"]);
    modify_fixture(&root);
    let delta = run_trace(binary, &root, &["-avvv", "--no-whole-file"]);
    vec![project(&fresh, &root_text), project(&delta, &root_text)]
}

#[test]
fn local_vvv_trace_matches_upstream_per_role() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let expected = trace_scenarios(upstream.binary());
    let actual = trace_scenarios(&oc_rsync_bin());
    for (scenario, (want, got)) in ["fresh copy", "delta"]
        .iter()
        .zip(expected.iter().zip(actual.iter()))
    {
        assert_eq!(
            got.receipt_counts, want.receipt_counts,
            "{scenario}: file-list receipt lines differ from upstream"
        );
        for (role, want_lines) in &want.sequences {
            assert_eq!(
                got.sequences.get(role).unwrap_or(&Vec::new()),
                want_lines,
                "{scenario}: {role:?} trace differs from upstream"
            );
        }
        assert_eq!(
            got.sequences.keys().collect::<Vec<_>>(),
            want.sequences.keys().collect::<Vec<_>>(),
            "{scenario}: oc prints roles upstream does not"
        );
    }
}

#[test]
fn run_specific_values_are_masked() {
    let root = "/tmp/fixture";
    assert_eq!(
        normalize("renaming sub/.big.bin.Z2tf4a to sub/big.bin", root),
        "renaming sub/.big.bin.XXXXXX to sub/big.bin"
    );
    assert_eq!(
        normalize("server_recv(2) starting pid=2658522", root),
        "server_recv(2) starting pid=<PID>"
    );
    assert_eq!(
        normalize(
            "sent 242 bytes  received 1,324 bytes  3,132.00 bytes/sec",
            root
        ),
        "sent <N> bytes  received <N> bytes  <RATE> bytes/sec"
    );
    assert_eq!(
        normalize("total size is 70,035  speedup is 44.72", root),
        "total size is 70,035  speedup is <N>"
    );
    assert_eq!(
        normalize("[sender] pushing local filters for /tmp/fixture/src/", root),
        "[sender] pushing local filters for <ROOT>/src/"
    );
}

#[test]
fn receipt_lines_compare_as_a_multiset_and_roles_as_sequences() {
    let a = project(
        "recv_file_name(x)\nrecv_generator(x,1)\nsend_files(1, src/x)\nrecv_file_name(x)\n",
        "/r",
    );
    let b = project(
        "send_files(1, src/x)\nrecv_file_name(x)\nrecv_file_name(x)\nrecv_generator(x,1)\n",
        "/r",
    );
    assert_eq!(
        a, b,
        "a different interleave of the same per-role lines must compare equal"
    );
    let reordered = project("recv_generator(y,2)\nrecv_generator(x,1)\n", "/r");
    let ordered = project("recv_generator(x,1)\nrecv_generator(y,2)\n", "/r");
    assert_ne!(reordered, ordered, "a role's own order must be significant");
}
