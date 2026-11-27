//! Oracle cells: `-H`/`--no-hard-links`, the device and special pairs and
//! `-D`/`--no-D` combine the way rsync 3.5.2 combines them.
//!
//! upstream: options.c:682-688 declare `--devices`, `--write-devices` and
//! `--specials` as POPT_ARG_VAL pairs with their `--no-` forms, so the last one
//! wins. `-a` and `-D` set devices and specials together and `--no-D` clears
//! both, each at its own position (options.c:1700-1709). `-H` is a counter that
//! `--no-hard-links`/`--no-H` reset (options.c:704-706, :1716-1717), and a count
//! above 1 doubles the `H` letter a client sends (options.c:2909-2913).
//! server_options() sends `D` for devices and `--no-specials`/`--specials` to
//! say what specials do relative to it (options.c:2918-2919, :3002-3007).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// What a run did, reduced to the parts these options decide.
#[derive(Debug, PartialEq)]
struct Outcome {
    /// `H` count and `D` presence in the server's option cluster on a push.
    letters: String,
    /// The long options server_options() adds for these flags on a push.
    long: Vec<String>,
    /// Exit code of a local copy.
    local_code: Option<i32>,
    /// Whether the local copy created the FIFO.
    fifo: bool,
    /// Link count of the copied hard-linked file, 0 when it was not copied.
    nlink: u64,
}

/// A source with a hard-linked pair and a FIFO, plus a remote shell that only
/// records the server command line.
fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/f"), b"a\n").unwrap();
    fs::hard_link(root.join("src/f"), root.join("src/g")).unwrap();
    let status = Command::new("mkfifo")
        .arg(root.join("src/p"))
        .status()
        .unwrap();
    assert!(status.success(), "mkfifo failed");
    let rsh = root.join("rsh");
    fs::write(
        &rsh,
        "#!/bin/sh\nshift\nprintf '%s\\n' \"$*\" > \"$(dirname \"$0\")/argv\"\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn run(binary: &Path, options: &[&str]) -> Outcome {
    let dir = fixture();
    let root = dir.path();
    Command::new(binary)
        .current_dir(root)
        .arg("-r")
        .args(options)
        .arg("-e")
        .arg(root.join("rsh"))
        .arg("src/")
        .arg(format!("lh:{}/dst/", root.display()))
        .output()
        .expect("run push");
    let argv = fs::read_to_string(root.join("argv")).expect("server argv recorded");
    let local = Command::new(binary)
        .current_dir(root)
        .arg("-r")
        .args(options)
        .args(["src/", "ldst/"])
        .output()
        .expect("run local copy");
    Outcome {
        letters: letters(&argv),
        long: long_options(&argv),
        local_code: local.status.code(),
        fifo: fs::symlink_metadata(root.join("ldst/p")).is_ok_and(|m| m.file_type().is_fifo()),
        nlink: fs::metadata(root.join("ldst/f")).map_or(0, |m| m.nlink()),
    }
}

/// The `H` and `D` letters of the server's option cluster, in that order.
fn letters(argv: &str) -> String {
    let cluster = argv
        .split_whitespace()
        .skip_while(|word| *word != "--server")
        .find(|word| word.starts_with('-') && !word.starts_with("--"))
        .unwrap_or_default();
    let cluster = cluster.split_once('e').map_or(cluster, |(head, _)| head);
    let hard_links = "H".repeat(cluster.matches('H').count());
    let devices = if cluster.contains('D') { "D" } else { "" };
    format!("{hard_links}{devices}")
}

fn long_options(argv: &str) -> Vec<String> {
    argv.split_whitespace()
        .filter(|word| matches!(*word, "--specials" | "--no-specials" | "--write-devices"))
        .map(str::to_owned)
        .collect()
}

fn outcome(letters: &str, long: &[&str], fifo: bool, nlink: u64) -> Outcome {
    Outcome {
        letters: letters.to_owned(),
        long: long.iter().map(|word| (*word).to_owned()).collect(),
        local_code: Some(0),
        fifo,
        nlink,
    }
}

/// Each option order and what rsync 3.5.2 does with it.
fn cells() -> Vec<(&'static [&'static str], Outcome)> {
    vec![
        (&["-H", "--no-hard-links"], outcome("", &[], false, 1)),
        (&["--no-hard-links", "-H"], outcome("H", &[], false, 2)),
        (&["-HH"], outcome("HH", &[], false, 2)),
        (&["-HH", "--no-H"], outcome("", &[], false, 1)),
        (&["--no-H", "-HH"], outcome("HH", &[], false, 2)),
        (&["--devices", "--no-devices"], outcome("", &[], false, 1)),
        (
            &["--no-devices", "--devices"],
            outcome("D", &["--no-specials"], false, 1),
        ),
        (&["--specials", "--no-specials"], outcome("", &[], false, 1)),
        (
            &["--no-specials", "--specials"],
            outcome("", &["--specials"], true, 1),
        ),
        (
            &["-D", "--no-devices"],
            outcome("", &["--specials"], true, 1),
        ),
        (&["--no-devices", "-D"], outcome("D", &[], true, 1)),
        (
            &["--no-D", "--specials"],
            outcome("", &["--specials"], true, 1),
        ),
        (&["--specials", "--no-D"], outcome("", &[], false, 1)),
        (&["-a", "--no-D"], outcome("", &[], false, 1)),
        (&["--no-D", "-a"], outcome("D", &[], true, 1)),
        (
            &["-a", "--no-specials"],
            outcome("D", &["--no-specials"], false, 1),
        ),
        (&["--no-specials", "-a"], outcome("D", &[], true, 1)),
        (
            &["--write-devices", "--no-write-devices"],
            outcome("", &[], false, 1),
        ),
        (
            &["--no-write-devices", "--write-devices"],
            outcome("", &["--write-devices"], false, 1),
        ),
    ]
}

#[test]
fn hard_link_and_device_options_combine_like_upstream() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for (options, expected) in cells() {
        assert_eq!(run(&oc_rsync_bin(), options), expected, "oc {options:?}");
        if let Some(upstream) = &upstream {
            assert_eq!(
                run(upstream.binary(), options),
                expected,
                "3.5.2 {options:?}"
            );
        }
    }
}
