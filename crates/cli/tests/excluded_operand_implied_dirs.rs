//! A name the filters exclude contributes nothing to the file list: not the
//! name, and not the parent directories it would imply under `--relative` or
//! `--files-from`.
//!
//! upstream: `send_file_list()` skips an excluded operand at flist.c:2969-2972
//! (`if (name_type != DOTDIR_NAME && st.st_mode != 0 && is_excluded(...))
//! continue;`), before `send_implied_dirs()` at flist.c:2982-2997. A sender
//! that lists those parents anyway makes the receiver reject the list:
//! `ERROR: rejecting excluded file-list name` (flist.c:1366-1370), exit 4.
//!
//! Every cell asserts the rsync 3.5.2 result measured on glibc. When the 3.5.2
//! oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under
//! `OC_RSYNC_UPSTREAM_COMPAT=1`), each cell also runs 3.5.2 at both ends and
//! with one end each, and all results must be equal.
//!
//! A daemon pull that names several `-R` operands has no cell: the names can
//! only reach a daemon one module operand each, and the client side of that is
//! a separate defect.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// How the names reach the sender.
#[derive(Clone, Copy, Debug)]
enum Form {
    /// `--files-from` naming them.
    FilesFrom,
    /// `-R` operands, relative to the source directory.
    Relative,
    /// `-R` operands that carry their own `/./` anchor.
    Anchored,
}

/// One rule set and the names sent under it.
struct Row {
    name: &'static str,
    rules: &'static [&'static str],
    options: &'static [&'static str],
    form: Form,
    names: &'static [&'static str],
    /// The sorted `--out-format=%n` lines of a local dry run.
    listing: &'static str,
    /// The destination tree after a local copy and after a push.
    tree: &'static str,
    /// The exit code and destination tree of a pull.
    pull: (i32, &'static str),
}

const ROWS: [Row; 11] = [
    Row {
        name: "excluded names",
        rules: &["- d/**"],
        options: &[],
        form: Form::FilesFrom,
        names: &["d/f", "I", "d/sub/f"],
        listing: "I",
        tree: "I",
        pull: (0, "I"),
    },
    Row {
        name: "excluded names",
        rules: &["- d/**"],
        options: &[],
        form: Form::Relative,
        names: &["d/f", "I", "d/sub/f"],
        listing: "I",
        tree: "I",
        pull: (0, "I"),
    },
    Row {
        name: "parent shared with a kept name",
        rules: &["- d/f"],
        options: &[],
        form: Form::FilesFrom,
        names: &["d/f", "d/keep"],
        listing: "d/ d/keep",
        tree: "d/ d/keep",
        pull: (0, "d/ d/keep"),
    },
    Row {
        name: "parent shared with a kept name",
        rules: &["- d/f"],
        options: &[],
        form: Form::Relative,
        names: &["d/f", "d/keep"],
        listing: "d/ d/keep",
        tree: "d/ d/keep",
        pull: (0, "d/ d/keep"),
    },
    Row {
        name: "parent shared, kept name first",
        rules: &["- d/f"],
        options: &[],
        form: Form::Relative,
        names: &["d/keep", "d/f"],
        listing: "d/ d/keep",
        tree: "d/ d/keep",
        pull: (0, "d/ d/keep"),
    },
    Row {
        name: "anchored operands",
        rules: &["- sub/f"],
        options: &[],
        form: Form::Anchored,
        names: &["d/./sub/f", "d/./keep"],
        listing: "keep",
        tree: "keep",
        pull: (0, "keep"),
    },
    Row {
        name: "anchored operands",
        rules: &["- sub/**"],
        options: &[],
        form: Form::Anchored,
        names: &["d/./sub/f", "d/./keep"],
        listing: "keep",
        tree: "keep",
        pull: (0, "keep"),
    },
    Row {
        name: "no implied dirs",
        rules: &["- d/f"],
        options: &["--no-implied-dirs"],
        form: Form::Relative,
        names: &["d/f", "d/sub/f"],
        listing: "d/sub/f",
        tree: "d/ d/sub/ d/sub/f",
        pull: (0, "d/ d/sub/ d/sub/f"),
    },
    Row {
        name: "no implied dirs",
        rules: &["- d/**"],
        options: &["--no-implied-dirs"],
        form: Form::FilesFrom,
        names: &["d/f", "I", "d/sub/f"],
        listing: "I",
        tree: "I",
        pull: (0, "I"),
    },
    Row {
        name: "excluded directory operand",
        rules: &["- d/sub/"],
        options: &["-r"],
        form: Form::Relative,
        names: &["d/sub", "I"],
        listing: "I",
        tree: "I",
        pull: (0, "I"),
    },
    // Control: the file is not excluded, only a parent directory's name
    // matches. Its parents are implied, and a receiver that holds the rule
    // rejects them, in rsync 3.5.2 as well.
    Row {
        name: "control: a parent matches, the name does not",
        rules: &["- sub/"],
        options: &[],
        form: Form::FilesFrom,
        names: &["d/f", "I", "d/sub/f"],
        listing: "I d/ d/f d/sub/ d/sub/f",
        tree: "I d/ d/f d/sub/ d/sub/f",
        pull: (4, ""),
    },
];

/// Which way the names travel.
#[derive(Clone, Copy, Debug)]
enum Leg {
    /// A local dry run; the result is its `--out-format=%n` lines.
    Listing,
    /// A local copy.
    Local,
    /// The client sends over a remote shell.
    Push,
    /// The server sends over a remote shell.
    Pull,
    /// The client sends to a daemon module. `--files-from` rows only.
    DaemonPush,
    /// A daemon module sends. `--files-from` rows only.
    DaemonPull,
}

impl Leg {
    const fn is_daemon(self) -> bool {
        matches!(self, Self::DaemonPush | Self::DaemonPull)
    }

    const fn is_remote(self) -> bool {
        !matches!(self, Self::Listing | Self::Local)
    }
}

/// The sorted entries below `root`, directories with a trailing slash.
fn tree(root: &Path) -> String {
    fn walk(dir: &Path, root: &Path, names: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .expect("below the root")
                .display()
                .to_string();
            if path.is_dir() {
                names.push(format!("{name}/"));
                walk(&path, root, names);
            } else {
                names.push(name);
            }
        }
    }
    let mut names = Vec::new();
    walk(root, root, &mut names);
    names.sort();
    names.join(" ")
}

/// Runs one transfer and returns its exit code with the dry-run listing or
/// the destination tree.
fn run(client: &Path, server: &Path, row: &Row, leg: Leg) -> (Option<i32>, String) {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    for name in ["I", "d/I", "d/f", "d/keep", "d/sub/f"] {
        let path = src.join(name);
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        fs::write(path, b"x\n").expect("write source file");
    }
    let list = dir.path().join("list");
    fs::write(&list, row.names.join("\n") + "\n").expect("write list");
    // The remote shell: drop the host argument and run the server command.
    let shell = dir.path().join("rsh");
    fs::write(&shell, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write shell");
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod shell");
    let out = dir.path().join("out");
    // The daemon, started through a remote shell: one module for each end.
    let daemon_shell = dir.path().join("daemon-rsh");
    if leg.is_daemon() {
        fs::create_dir(&out).expect("mkdir out");
        let conf = dir.path().join("rsyncd.conf");
        fs::write(
            &conf,
            format!(
                "use chroot = no\n[src]\npath = {}\n[out]\npath = {}\nread only = no\n",
                src.display(),
                out.display()
            ),
        )
        .expect("write config");
        fs::write(
            &daemon_shell,
            format!(
                "#!/bin/sh\nexec \"{}\" --config={} --server --daemon .\n",
                server.display(),
                conf.display()
            ),
        )
        .expect("write daemon shell");
        fs::set_permissions(&daemon_shell, fs::Permissions::from_mode(0o755))
            .expect("chmod daemon shell");
    }

    let mut command = Command::new(client);
    command
        .current_dir(&src)
        .env("SSH_CONNECTION", "127.0.0.1 40000 127.0.0.1 22");
    for rule in row.rules {
        command.arg(format!("--filter={rule}"));
    }
    command.args(row.options);
    match leg {
        Leg::Listing => {
            command.args(["-n", "--out-format=%n"]);
        }
        Leg::Local => {}
        Leg::Push | Leg::Pull => {
            command
                .arg("-e")
                .arg(&shell)
                .arg(format!("--rsync-path={}", server.display()));
        }
        Leg::DaemonPush | Leg::DaemonPull => {
            command.arg("-e").arg(&daemon_shell);
        }
    }
    let source = match leg {
        Leg::Pull => format!("h:{}", src.display()),
        Leg::DaemonPull => "h::src".to_owned(),
        _ => src.display().to_string(),
    };
    let destination = match leg {
        Leg::Push => format!("h:{}/", out.display()),
        Leg::DaemonPush => "h::out/".to_owned(),
        _ => format!("{}/", out.display()),
    };
    match row.form {
        Form::FilesFrom => {
            command
                .arg(format!("--files-from={}", list.display()))
                .arg(format!("{source}/"));
        }
        Form::Relative => {
            command.arg("-R");
            for name in row.names {
                match leg {
                    Leg::Pull => command.arg(format!("{source}/./{name}")),
                    _ => command.arg(name),
                };
            }
        }
        Form::Anchored => {
            command.arg("-R");
            for name in row.names {
                match leg {
                    Leg::Pull => command.arg(format!("{source}/{name}")),
                    _ => command.arg(name),
                };
            }
        }
    }
    let output = command.arg(destination).output().expect("run rsync");
    let result = match leg {
        Leg::Listing => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut lines: Vec<&str> = stdout
                .lines()
                .filter(|line| !line.is_empty() && !line.starts_with("created directory"))
                .collect();
            lines.sort_unstable();
            lines.join(" ")
        }
        _ => tree(&out),
    };
    (output.status.code(), result)
}

/// Checks one leg of every row: oc at both ends gives the measured 3.5.2
/// result, and so does every pairing with 3.5.2.
fn check(leg: Leg) {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    let oc = oc_rsync_bin();
    for row in &ROWS {
        if leg.is_daemon() && !matches!(row.form, Form::FilesFrom) {
            continue;
        }
        let expected = match leg {
            Leg::Listing => (Some(0), row.listing.to_owned()),
            Leg::Local | Leg::Push | Leg::DaemonPush => (Some(0), row.tree.to_owned()),
            Leg::Pull | Leg::DaemonPull => (Some(row.pull.0), row.pull.1.to_owned()),
        };
        let cell = format!(
            "{leg:?} / {} {:?} {:?} {:?} {:?}",
            row.name, row.rules, row.options, row.form, row.names
        );
        assert_eq!(run(&oc, &oc, row, leg), expected, "{cell}: oc to oc");
        let Some(upstream) = upstream.as_ref() else {
            continue;
        };
        let reference = upstream.binary();
        assert_eq!(
            run(reference, reference, row, leg),
            expected,
            "{cell}: rsync 3.5.2 to rsync 3.5.2"
        );
        if leg.is_remote() {
            assert_eq!(
                run(reference, &oc, row, leg),
                expected,
                "{cell}: rsync 3.5.2 client, oc server"
            );
            assert_eq!(
                run(&oc, reference, row, leg),
                expected,
                "{cell}: oc client, rsync 3.5.2 server"
            );
        }
    }
}

#[test]
fn a_dry_run_lists_no_parent_of_an_excluded_name() {
    check(Leg::Listing);
}

#[test]
fn a_local_copy_creates_no_parent_of_an_excluded_name() {
    check(Leg::Local);
}

#[test]
fn a_push_creates_no_parent_of_an_excluded_name() {
    check(Leg::Push);
}

#[test]
fn a_daemon_push_creates_no_parent_of_an_excluded_name() {
    check(Leg::DaemonPush);
}

#[test]
fn a_daemon_pull_sends_no_parent_of_an_excluded_name() {
    check(Leg::DaemonPull);
}

/// The interoperability case: with the parents of an excluded name in the file
/// list, an rsync 3.5.2 client pulling from an oc server rejects the list and
/// exits 4.
#[test]
fn a_pull_sends_no_parent_of_an_excluded_name() {
    check(Leg::Pull);
}
