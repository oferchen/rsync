//! The receiver refuses two destinations before it transfers anything.
//!
//! - `--backup-dir`: upstream: main.c:1036-1057 do_recv() stats the backup
//!   directory, resolved against the destination. A path that is not a
//!   directory exits `RERR_SYNTAX` (1), any stat failure but `ENOENT` exits
//!   `RERR_FILEIO` (11).
//! - An existing destination directory the receiver cannot enter: upstream:
//!   main.c:776-781 get_local_name() reports `change_dir#1 %s failed` and exits
//!   `RERR_FILESELECT` (3), dry run or not.
//!
//! Every cell asserts the rsync 3.5.2 result measured on glibc. When the 3.5.2
//! oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under
//! `OC_RSYNC_UPSTREAM_COMPAT=1`), each cell also runs 3.5.2 at both ends and
//! with one end each, and all results must be equal.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// Where the receiver runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leg {
    Local,
    Push,
    Pull,
    DaemonPush,
}

impl Leg {
    fn is_remote(self) -> bool {
        self != Self::Local
    }
}

/// One transfer setup: a source, a destination holding an older `f`, and the
/// remote shells that start the other end.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let base = dir.path();
        fs::create_dir(base.join("src")).expect("mkdir src");
        fs::write(base.join("src/f"), b"new\n").expect("write source");
        fs::create_dir(base.join("out")).expect("mkdir out");
        fs::write(base.join("out/f"), b"old\n").expect("write destination");
        fs::write(base.join("out/reg"), b"x\n").expect("write regular file");
        fs::write(base.join("reg"), b"x\n").expect("write regular file");
        fs::write(base.join("rsh"), "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write shell");
        fs::set_permissions(base.join("rsh"), fs::Permissions::from_mode(0o755))
            .expect("chmod shell");
        Self { dir }
    }

    fn base(&self) -> &Path {
        self.dir.path()
    }

    fn out(&self) -> PathBuf {
        self.base().join("out")
    }

    /// A shell that starts `server` as a daemon serving `out` as module `out`.
    fn daemon_shell(&self, server: &Path) -> PathBuf {
        let conf = self.base().join("rsyncd.conf");
        fs::write(
            &conf,
            format!(
                "use chroot = no\n[out]\npath = {}\nread only = no\n",
                self.out().display()
            ),
        )
        .expect("write config");
        let shell = self.base().join("daemon-rsh");
        fs::write(
            &shell,
            format!(
                "#!/bin/sh\nexec \"{}\" --config={} --server --daemon .\n",
                server.display(),
                conf.display()
            ),
        )
        .expect("write daemon shell");
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod shell");
        shell
    }

    /// Runs `client -a <options> src/ out/` over `leg` and returns the exit
    /// code with stderr.
    fn run(
        &self,
        client: &Path,
        server: &Path,
        leg: Leg,
        options: &[String],
    ) -> (Option<i32>, String) {
        let base = self.base();
        let mut command = Command::new(client);
        command.arg("-a").args(options);
        let (source, destination) = match leg {
            Leg::Local => (
                format!("{}/src/", base.display()),
                format!("{}/", self.out().display()),
            ),
            Leg::Push => (
                format!("{}/src/", base.display()),
                format!("h:{}/", self.out().display()),
            ),
            Leg::Pull => (
                format!("h:{}/src/", base.display()),
                format!("{}/", self.out().display()),
            ),
            Leg::DaemonPush => (format!("{}/src/", base.display()), "h::out/".to_owned()),
        };
        match leg {
            Leg::Local => {}
            Leg::Push | Leg::Pull => {
                command
                    .arg("-e")
                    .arg(base.join("rsh"))
                    .arg(format!("--rsync-path={}", server.display()));
            }
            Leg::DaemonPush => {
                command.arg("-e").arg(self.daemon_shell(server));
            }
        }
        let output = command
            .arg(source)
            .arg(destination)
            .output()
            .expect("run rsync");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn is_root(&self) -> bool {
        fs::metadata(self.base()).expect("stat tempdir").uid() == 0
    }
}

/// One refusal: the options, the legs it applies to, and what 3.5.2 does.
struct Cell {
    name: &'static str,
    /// `--backup-dir` value; `{base}` is replaced by the fixture directory.
    backup_dir: &'static str,
    legs: &'static [Leg],
    code: i32,
    /// The diagnostic up to the strerror text, which is libc-specific.
    message: &'static str,
}

const ALL_LEGS: &[Leg] = &[Leg::Local, Leg::Push, Leg::Pull, Leg::DaemonPush];
// A daemon module sanitizes an absolute --backup-dir (options.c:2473), so only
// the destination-relative spellings reach a daemon unchanged.
const SHELL_LEGS: &[Leg] = &[Leg::Local, Leg::Push, Leg::Pull];

const BACKUP_CELLS: [Cell; 4] = [
    Cell {
        name: "relative path below a regular file",
        backup_dir: "reg/sub",
        legs: ALL_LEGS,
        code: 11,
        message: "Failed to stat reg/sub: ",
    },
    Cell {
        name: "relative regular file with a trailing slash",
        backup_dir: "./reg/",
        legs: ALL_LEGS,
        code: 1,
        message: "The backup-dir is not a directory: reg",
    },
    Cell {
        name: "absolute path below a regular file",
        backup_dir: "{base}/reg/sub",
        legs: SHELL_LEGS,
        code: 11,
        message: "Failed to stat {base}/reg/sub: ",
    },
    Cell {
        name: "absolute regular file",
        backup_dir: "{base}/reg",
        legs: SHELL_LEGS,
        code: 1,
        message: "The backup-dir is not a directory: {base}/reg",
    },
];

/// The client/server pairs a cell runs: oc at both ends, then every pairing
/// with 3.5.2 when its binary resolves.
fn pairs(leg: Leg) -> Vec<(String, PathBuf, PathBuf)> {
    let oc = oc_rsync_bin();
    let mut pairs = vec![("oc to oc".to_owned(), oc.clone(), oc.clone())];
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let reference = upstream.binary().to_path_buf();
        pairs.push((
            "rsync 3.5.2 to rsync 3.5.2".to_owned(),
            reference.clone(),
            reference.clone(),
        ));
        if leg.is_remote() {
            pairs.push((
                "rsync 3.5.2 client, oc server".to_owned(),
                reference.clone(),
                oc.clone(),
            ));
            pairs.push(("oc client, rsync 3.5.2 server".to_owned(), oc, reference));
        }
    }
    pairs
}

fn check_backup_dir(leg: Leg) {
    for cell in BACKUP_CELLS.iter().filter(|cell| cell.legs.contains(&leg)) {
        for (pair, client, server) in pairs(leg) {
            let fixture = Fixture::new();
            let base = fixture.base().display().to_string();
            let option = format!("--backup-dir={}", cell.backup_dir.replace("{base}", &base));
            let (code, stderr) = fixture.run(&client, &server, leg, &[option]);
            let case = format!("{leg:?} / {} / {pair}", cell.name);
            assert_eq!(code, Some(cell.code), "{case}: {stderr}");
            let message = cell.message.replace("{base}", &base);
            assert!(
                stderr.contains(&message),
                "{case}: expected {message:?} in {stderr}"
            );
            assert_eq!(
                fs::read(fixture.out().join("f")).expect("read destination"),
                b"old\n",
                "{case}: the receiver transferred before refusing"
            );
        }
    }
}

/// An existing destination root with mode 000 cannot be entered.
fn check_unsearchable_root(leg: Leg, options: &[String]) {
    for (pair, client, server) in pairs(leg) {
        let fixture = Fixture::new();
        if fixture.is_root() {
            eprintln!("SKIPPED {leg:?} unsearchable root: root ignores mode 000");
            return;
        }
        let out = fixture.out();
        fs::set_permissions(&out, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        let (code, stderr) = fixture.run(&client, &server, leg, options);
        fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).expect("chmod 755");
        let case = format!("{leg:?} {options:?} unsearchable root / {pair}");
        assert_eq!(code, Some(3), "{case}: {stderr}");
        // The local copy is still worded `failed to access destination
        // directory`; the remote receivers carry upstream's line.
        if leg != Leg::Local {
            let message = format!("change_dir#1 \"{}/\" failed: ", out.display());
            assert!(
                stderr.contains(&message),
                "{case}: expected {message:?} in {stderr}"
            );
        }
        assert_eq!(
            fs::read(out.join("f")).expect("read destination"),
            b"old\n",
            "{case}: the receiver transferred into an unsearchable root"
        );
    }
}

#[test]
fn a_local_copy_refuses_a_backup_dir_that_is_not_a_directory() {
    check_backup_dir(Leg::Local);
}

#[test]
fn a_push_refuses_a_backup_dir_that_is_not_a_directory() {
    check_backup_dir(Leg::Push);
}

#[test]
fn a_pull_refuses_a_backup_dir_that_is_not_a_directory() {
    check_backup_dir(Leg::Pull);
}

#[test]
fn a_daemon_push_refuses_a_backup_dir_that_is_not_a_directory() {
    check_backup_dir(Leg::DaemonPush);
}

#[test]
fn a_local_copy_refuses_an_unsearchable_destination_root() {
    check_unsearchable_root(Leg::Local, &[]);
}

#[test]
fn a_push_refuses_an_unsearchable_destination_root() {
    check_unsearchable_root(Leg::Push, &[]);
}

#[test]
fn a_pull_refuses_an_unsearchable_destination_root() {
    check_unsearchable_root(Leg::Pull, &[]);
}

#[test]
fn a_dry_run_pull_refuses_an_unsearchable_destination_root() {
    check_unsearchable_root(Leg::Pull, &["--dry-run".to_owned()]);
}
