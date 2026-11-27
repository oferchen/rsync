//! Oracle cells: `-l`, `--no-links` and `-L` combine the way rsync 3.5.2
//! combines them.
//!
//! upstream: options.c:692-695 - `--links` and `--no-links` are a
//! POPT_ARG_VAL pair on `preserve_links`, so the last one wins, while
//! `--copy-links` is a separate POPT_ARG_NONE that nothing in parse_arguments()
//! refuses alongside either. A sender with `copy_links` stats through every
//! symlink (flist.c:517, :541), so `-L` wins over `-l` and the receiver gets a
//! file. server_options() forwards `l` whenever `preserve_links` is set
//! (options.c:2877-2878) but `L` only to a remote sender (options.c:2896-2898).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// How a transfer reaches the destination.
#[derive(Clone, Copy, Debug)]
enum Mode {
    Local,
    Push,
    Pull,
}

/// What the destination holds at `link`, and what the run reported.
#[derive(Debug, PartialEq)]
struct Outcome {
    code: Option<i32>,
    entry: &'static str,
    stdout: Vec<String>,
    stderr: Vec<String>,
    /// The `l` and `L` letters of the server's option cluster, or `None` for a
    /// local copy, which starts no server.
    server_letters: Option<String>,
}

/// A source holding `target` and a relative `link` to it, plus a remote shell
/// that records the server command line and runs it on this host.
fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/target"), b"data\n").unwrap();
    symlink("target", root.join("src/link")).unwrap();
    let rsh = root.join("rsh");
    fs::write(
        &rsh,
        "#!/bin/sh\nshift\nprintf '%s\\n' \"$*\" > \"$(dirname \"$0\")/argv\"\nexec \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn run(binary: &Path, options: &[&str], mode: Mode) -> Outcome {
    let dir = fixture();
    let root = dir.path().display().to_string();
    let mut command = Command::new(binary);
    command.current_dir(dir.path()).arg("-r").args(options);
    let (from, to) = match mode {
        Mode::Local => ("src/".to_owned(), format!("{root}/dst/")),
        Mode::Push => ("src/".to_owned(), format!("lh:{root}/dst/")),
        Mode::Pull => (format!("lh:{root}/src/"), format!("{root}/dst/")),
    };
    if !matches!(mode, Mode::Local) {
        command
            .arg("-e")
            .arg(dir.path().join("rsh"))
            .arg(format!("--rsync-path={}", binary.display()));
    }
    let output = command.args([from, to]).output().expect("run rsync");
    Outcome {
        code: output.status.code(),
        entry: entry_kind(&dir.path().join("dst/link")),
        stdout: lines(&output.stdout),
        stderr: lines(&output.stderr),
        server_letters: fs::read_to_string(dir.path().join("argv"))
            .ok()
            .map(|argv| link_letters(&argv)),
    }
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| line.strip_prefix("oc-").unwrap_or(line).to_owned())
        .collect()
}

fn entry_kind(path: &Path) -> &'static str {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => "symlink",
        Ok(meta) if meta.is_file() => "file",
        Ok(_) => "other",
        Err(_) => "absent",
    }
}

/// The `l` and `L` letters of the first single-dash cluster after `--server`,
/// in that order. The rest of the cluster differs between implementations.
fn link_letters(argv: &str) -> String {
    let cluster = argv
        .split_whitespace()
        .skip_while(|word| *word != "--server")
        .find(|word| word.starts_with('-') && !word.starts_with("--"))
        .unwrap_or_default();
    let cluster = cluster.split_once('e').map_or(cluster, |(head, _)| head);
    ['l', 'L']
        .into_iter()
        .filter(|letter| cluster.contains(*letter))
        .collect()
}

/// upstream: generator.c:2154 reports a skipped symlink through FINFO, so the line
/// lands on stdout and stderr stays empty.
fn outcome(code: i32, entry: &'static str, stdout: &[&str], letters: Option<&str>) -> Outcome {
    Outcome {
        code: Some(code),
        entry,
        stdout: stdout.iter().map(|line| (*line).to_owned()).collect(),
        stderr: Vec::new(),
        server_letters: letters.map(str::to_owned),
    }
}

const SKIPPED: &[&str] = &["skipping non-regular file \"link\""];

/// Each option order, and the outcome 3.5.2 produces for a local copy, a push
/// and a pull.
fn cells() -> Vec<(&'static [&'static str], [Outcome; 3])> {
    let copied = |push: &'static str, pull: &'static str| {
        [
            outcome(0, "file", &[], None),
            outcome(0, "file", &[], Some(push)),
            outcome(0, "file", &[], Some(pull)),
        ]
    };
    vec![
        (&["-lL"], copied("l", "lL")),
        (&["-Ll"], copied("l", "lL")),
        (&["-L", "--no-links"], copied("", "L")),
        (&["--no-links", "-L"], copied("", "L")),
        (
            &["-l", "--no-links"],
            [
                outcome(0, "absent", SKIPPED, None),
                outcome(0, "absent", SKIPPED, Some("")),
                outcome(0, "absent", SKIPPED, Some("")),
            ],
        ),
        (
            &["--no-links", "-l"],
            [
                outcome(0, "symlink", &[], None),
                outcome(0, "symlink", &[], Some("l")),
                outcome(0, "symlink", &[], Some("l")),
            ],
        ),
    ]
}

#[test]
fn link_options_combine_like_upstream() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for (options, expected) in cells() {
        for (mode, expected) in [Mode::Local, Mode::Push, Mode::Pull]
            .into_iter()
            .zip(expected)
        {
            let cell = format!("{options:?} {mode:?}");
            assert_eq!(run(&oc_rsync_bin(), options, mode), expected, "oc {cell}");
            if let Some(upstream) = &upstream {
                assert_eq!(
                    run(upstream.binary(), options, mode),
                    expected,
                    "3.5.2 {cell}"
                );
            }
        }
    }
}
