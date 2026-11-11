//! A pull client names a file only once its temp file has opened, as
//! upstream's receiver does (receiver.c:1254-1264: a failed `open_tmpfile()`
//! `continue`s before `log_item()`). A file whose temp cannot be created shows
//! its mkstemp error and no name line.
//!
//! Directory and file lines may interleave differently from upstream, so the
//! cell compares each file's own lines, not a total order.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const FILES: [&str; 3] = ["a.txt", "sub/b.txt", "z.txt"];

fn pull(root: &Path, temp_dir: &Path) -> Output {
    let bin = test_support::oc_rsync_bin();
    Command::new(&bin)
        .current_dir(root)
        .args(["-rv", "-T"])
        .arg(temp_dir)
        .arg("-e")
        .arg(root.join("rsh"))
        .arg(format!("--rsync-path={}", bin.display()))
        .arg(format!("lh:{}/src/", root.display()))
        .arg("dst/")
        .output()
        .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src/sub")).unwrap();
    for name in FILES {
        fs::write(root.join("src").join(name), name).unwrap();
    }
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn merged(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

fn mkstemp_lines<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let base = Path::new(name).file_name().unwrap().to_str().unwrap();
    text.lines()
        .filter(|line| {
            line.starts_with("rsync: [receiver] mkstemp ")
                && line.contains(&format!("/{base}."))
                && line.ends_with("failed: Permission denied (13)")
        })
        .collect()
}

#[test]
fn a_pulled_file_whose_temp_cannot_open_prints_its_error_and_no_name() {
    let dir = fixture();
    let root = dir.path();
    let temp = root.join("tmp");
    fs::create_dir(&temp).unwrap();
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(temp.join("probe"), b"").is_ok() {
        // Root ignores the mode bits, so no open can fail here.
        return;
    }

    let output = pull(root, &temp);
    let text = merged(&output);
    assert_eq!(output.status.code(), Some(23), "{text}");
    for name in FILES {
        assert!(
            !text.lines().any(|line| line == name),
            "{name} was named although its temp never opened:\n{text}"
        );
        assert_eq!(mkstemp_lines(&text, name).len(), 1, "{name}:\n{text}");
    }
    assert!(text.lines().any(|line| line == "sub/"), "{text}");
}

/// Control: the same pull with a writable temp dir names every file and
/// reports no mkstemp error, so the cell above cannot pass by never naming.
#[test]
fn a_pulled_file_whose_temp_opens_is_named() {
    let dir = fixture();
    let root = dir.path();
    let temp = root.join("tmp");
    fs::create_dir(&temp).unwrap();

    let output = pull(root, &temp);
    let text = merged(&output);
    assert!(output.status.success(), "{text}");
    for name in FILES {
        assert!(text.lines().any(|line| line == name), "{name}:\n{text}");
        assert!(mkstemp_lines(&text, name).is_empty(), "{name}:\n{text}");
    }
}
