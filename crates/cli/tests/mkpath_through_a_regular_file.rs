//! `--mkpath` into a destination whose chain holds a regular file.
//!
//! upstream: main.c:757-761 get_local_name() runs make_path() when the
//! destination does not stat, and a failure jumps to `mkdir_error`
//! (main.c:818-821): `mkdir %s failed` with make_path()'s own errno, then
//! `exit_cleanup(RERR_FILEIO)`. make_path() (util1.c:231-247) tries the
//! deepest directory first and climbs only on `ENOENT`, so the errno is that
//! deepest mkdir's:
//!
//! - `EEXIST` when the regular file is the deepest directory itself;
//! - `ENOTDIR` when it is that directory's parent;
//! - for a relative destination, `ELOOP` when it sits higher still: the
//!   receiver's do_mkdir_at() opens the parent with secure_relative_dirfd(),
//!   which reports a non-directory intermediate as `ELOOP`
//!   (syscall.c:3279-3281). An absolute destination takes plain mkdir()
//!   (syscall.c:2309-2310) and keeps `ENOTDIR`.
//!
//! Every cell asserts the rsync 3.5.2 result. When the 3.5.2 oracle resolves
//! (`OC_RSYNC_UPSTREAM_BIN_3_5_2`), each remote cell also runs 3.5.2 at both
//! ends and with one end each, and all results must be equal.
#![cfg(unix)]

use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
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
}

/// How the destination operand is spelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spelling {
    /// Relative to the receiver's cwd, the fixture directory.
    Relative,
    /// The fixture directory joined in front.
    Absolute,
}

/// The errno a cell expects.
#[derive(Clone, Copy, Debug)]
enum Errno {
    Exists,
    NotADirectory,
    Loop,
}

/// One destination shape.
struct Cell {
    /// Recurse over `src/` instead of copying the single file `src/f`.
    recursive: bool,
    /// The destination below the fixture, `dst/dstfile` being a regular file.
    destination: &'static str,
    relative: Errno,
    absolute: Errno,
}

const CELLS: [Cell; 6] = [
    Cell {
        recursive: false,
        destination: "dst/dstfile/x",
        relative: Errno::Exists,
        absolute: Errno::Exists,
    },
    Cell {
        recursive: false,
        destination: "dst/dstfile/x/",
        relative: Errno::NotADirectory,
        absolute: Errno::NotADirectory,
    },
    Cell {
        recursive: false,
        destination: "dst/dstfile/x/y/",
        relative: Errno::Loop,
        absolute: Errno::NotADirectory,
    },
    Cell {
        recursive: false,
        destination: "dst/dstfile/x/y",
        relative: Errno::NotADirectory,
        absolute: Errno::NotADirectory,
    },
    Cell {
        recursive: true,
        destination: "dst/dstfile/x",
        relative: Errno::NotADirectory,
        absolute: Errno::NotADirectory,
    },
    Cell {
        recursive: true,
        destination: "dst/dstfile/x/y/",
        relative: Errno::Loop,
        absolute: Errno::NotADirectory,
    },
];

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let base = dir.path();
        fs::create_dir_all(base.join("src/sub")).expect("mkdir src");
        fs::write(base.join("src/f"), b"a\n").expect("write src/f");
        fs::write(base.join("src/sub/g"), b"b\n").expect("write src/sub/g");
        fs::create_dir(base.join("dst")).expect("mkdir dst");
        fs::write(base.join("dst/dstfile"), b"c\n").expect("write dstfile");
        fs::write(base.join("rsh"), "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write shell");
        fs::set_permissions(base.join("rsh"), fs::Permissions::from_mode(0o755))
            .expect("chmod shell");
        Self { dir }
    }

    fn base(&self) -> &Path {
        self.dir.path()
    }

    /// `mkdir "<the destination as the receiver names it>" failed: <strerror> (<n>)`.
    ///
    /// upstream: util1.c:1540-1547 full_fname() prefixes a relative name with
    /// curr_dir, which util1.c:1321 fills from getcwd() and so is the canonical
    /// directory (on macOS /var is a symlink to /private/var). An absolute name
    /// is printed as given.
    fn expected_line(&self, destination: &str, errno: Errno, spelling: Spelling) -> String {
        let base = match spelling {
            Spelling::Relative => fs::canonicalize(self.base()).expect("canonical fixture root"),
            Spelling::Absolute => self.base().to_path_buf(),
        };
        format!(
            "mkdir \"{}/{destination}\" failed: {}",
            base.display(),
            logging::upstream_errno_text(&self.kernel_error(errno))
        )
    }

    /// The kernel's own error for `errno`, produced by the call that fails
    /// that way, so no strerror text or errno number is written down here.
    fn kernel_error(&self, errno: Errno) -> io::Error {
        let base = self.base();
        match errno {
            Errno::Exists => fs::create_dir(base.join("dst/dstfile")),
            Errno::NotADirectory => fs::create_dir(base.join("dst/dstfile/x")),
            Errno::Loop => {
                let looped = base.join("loop");
                symlink(&looped, &looped).expect("plant a self loop");
                let error = fs::metadata(&looped).map(|_| ());
                fs::remove_file(&looped).expect("remove the loop");
                error
            }
        }
        .expect_err("the kernel refuses")
    }

    /// Runs `client --mkpath` for `cell` over `leg` from the fixture directory.
    fn run(
        &self,
        client: &Path,
        server: &Path,
        leg: Leg,
        cell: &Cell,
        spelling: Spelling,
    ) -> (Option<i32>, String) {
        let base = self.base();
        let destination = match spelling {
            Spelling::Relative => cell.destination.to_owned(),
            Spelling::Absolute => format!("{}/{}", base.display(), cell.destination),
        };
        let source = if cell.recursive { "src/" } else { "src/f" };
        let mut command = Command::new(client);
        command.current_dir(base).arg("--mkpath");
        if cell.recursive {
            command.arg("-r");
        }
        if leg != Leg::Local {
            command
                .arg("-e")
                .arg(base.join("rsh"))
                .arg(format!("--rsync-path={}", server.display()));
        }
        match leg {
            Leg::Local => command.arg(source).arg(destination),
            Leg::Push => command.arg(source).arg(format!("h:{destination}")),
            Leg::Pull => command
                .arg(format!("h:{}/{source}", base.display()))
                .arg(destination),
        };
        let output = command.output().expect("run rsync");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

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
        if leg != Leg::Local {
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

fn check(leg: Leg) {
    for spelling in [Spelling::Relative, Spelling::Absolute] {
        for cell in &CELLS {
            let errno = match spelling {
                Spelling::Relative => cell.relative,
                Spelling::Absolute => cell.absolute,
            };
            for (pair, client, server) in pairs(leg) {
                let fixture = Fixture::new();
                let (code, stderr) = fixture.run(&client, &server, leg, cell, spelling);
                let case = format!(
                    "{leg:?} / {spelling:?} / -r={} {} / {pair}",
                    cell.recursive, cell.destination
                );
                assert_eq!(code, Some(11), "{case}: {stderr}");
                let line = fixture.expected_line(cell.destination, errno, spelling);
                assert!(
                    stderr.contains(&line),
                    "{case}: expected {line:?} in {stderr}"
                );
                assert!(
                    !stderr.contains("failed to inspect destination"),
                    "{case}: {stderr}"
                );
                assert!(
                    fixture.base().join("dst/dstfile").is_file(),
                    "{case}: the regular file was replaced"
                );
            }
        }
    }
}

#[test]
fn local_copy_reports_make_paths_own_errno() {
    check(Leg::Local);
}

#[test]
fn push_receiver_reports_make_paths_own_errno() {
    check(Leg::Push);
}

#[test]
fn pull_receiver_reports_make_paths_own_errno() {
    check(Leg::Pull);
}
