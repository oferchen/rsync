//! A client writes each diagnostic the moment it is produced, interleaved with
//! its file names, as upstream's `rwrite()` does (log.c:272-373).
//!
//! The push runs over a local remote shell so the client is the sender and its
//! own `--debug=SEND` trace shares stdout with its `-v` name lines.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn line_index(lines: &[&str], wanted: &str) -> usize {
    lines
        .iter()
        .position(|line| *line == wanted)
        .unwrap_or_else(|| panic!("missing line {wanted:?} in:\n{}", lines.join("\n")))
}

#[test]
fn a_push_prints_each_send_trace_line_among_the_names_it_belongs_to() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("aaa.txt"), b"alpha\n").unwrap();
    fs::write(src.join("bbb.txt"), b"bravo\n").unwrap();
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();

    let bin = test_support::oc_rsync_bin();
    let output = Command::new(&bin)
        .current_dir(root)
        .args(["-rv", "--debug=send1", "-e"])
        .arg(&rsh)
        .arg(format!("--rsync-path={}", bin.display()))
        .arg("src/")
        .arg(format!("lh:{}/dst/", root.display()))
        .output()
        .unwrap();
    assert!(output.status.success(), "push failed: {output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();

    // A buffered trace would print every send_files line after both names.
    let first_trace = lines
        .iter()
        .position(|line| line.starts_with("send_files(") && line.ends_with("aaa.txt)"))
        .unwrap_or_else(|| panic!("no send_files line for aaa.txt in:\n{stdout}"));
    let second_trace = lines
        .iter()
        .position(|line| line.starts_with("send_files(") && line.ends_with("bbb.txt)"))
        .unwrap_or_else(|| panic!("no send_files line for bbb.txt in:\n{stdout}"));
    let first_name = line_index(&lines, "aaa.txt");
    assert!(
        first_trace < first_name && first_name < second_trace,
        "trace and names out of production order:\n{stdout}"
    );
}
