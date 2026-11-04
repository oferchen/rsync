//! A source tree holding a directory that is readable but not searchable
//! (mode `0400`), copied locally and pushed.
//!
//! rsync 3.5.1 chooses between two scans (`flist.c:2355-2361`). The default
//! is `secure_opendir()`: a traversal-only descriptor, then
//! `openat(fd, ".", O_RDONLY | O_DIRECTORY)` (`flist.c:2257-2295`,
//! `syscall.c:3172`, `:3218`). Resolving `"."` needs the directory's search
//! bit, so the scan fails with `opendir ... failed: Permission denied`, even
//! for an empty directory that 3.5.0 copied silently. The symlink-following
//! modes and the `--insecure-links` opt-out keep the legacy `opendir()`, and
//! `--files-from` opens its leaf readable (`syscall.c:593-605`). Those scans
//! list the directory, and each entry's own stat then fails as
//! `readlink_stat(...) failed` (`flist.c:1693`). `-d` without `-r` never
//! opens a subdirectory at all (`flist.c:2992`).
//!
//! The fixture is `s/b` (0400, holding `inner`), `s/x` (0400, empty) and
//! `s/c/g`, copied locally and pushed over a remote-shell shim. Every cell asserts the exact
//! sender diagnostics, the exit code, and that the readable sibling still
//! lands. Each expectation was measured against the rsync 3.5.1 tarball
//! build, and the oracle test re-measures it against that binary when one is
//! installed.
//!
//! Skip conditions: running as root, which ignores the directory modes. The
//! oracle cell also needs upstream 3.5.1 (`test_support::require_upstream_rsync`).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const RUN_TIMEOUT: Duration = Duration::from_secs(60);

const OPENDIR_B: &str = "rsync: [sender] opendir \"W/s/b\" failed: Permission denied (13)";
const OPENDIR_X: &str = "rsync: [sender] opendir \"W/s/x\" failed: Permission denied (13)";
const READLINK_INNER: &str =
    "rsync: [sender] readlink_stat(\"W/s/b/inner\") failed: Permission denied (13)";

/// One measured cell: the options, the sorted sender diagnostics and the exit code.
struct Cell {
    opts: &'static [&'static str],
    lines: &'static [&'static str],
    code: i32,
}

const CELLS: &[Cell] = &[
    Cell {
        opts: &["-r"],
        lines: &[OPENDIR_B, OPENDIR_X],
        code: 23,
    },
    Cell {
        opts: &["-r", "--copy-links"],
        lines: &[READLINK_INNER],
        code: 23,
    },
    Cell {
        opts: &["-r", "--insecure-links"],
        lines: &[READLINK_INNER],
        code: 23,
    },
    Cell {
        opts: &["-r", "--files-from=list"],
        lines: &[READLINK_INNER],
        code: 23,
    },
    Cell {
        opts: &["-d"],
        lines: &[],
        code: 0,
    },
];

/// The binary under test, from the compile-time path Cargo built it at.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// Root ignores the directory modes the fixture relies on.
fn is_root() -> bool {
    match Command::new("id").arg("-u").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "0",
        _ => false,
    }
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// The fixture tree, with its modes restored on drop so `TempDir` can remove it.
struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let src = temp.path().join("s");
        for dir in ["b", "x", "c"] {
            fs::create_dir_all(src.join(dir)).expect("mkdir");
        }
        fs::write(src.join("b/inner"), b"z\n").expect("write inner");
        fs::write(src.join("c/g"), b"y\n").expect("write g");
        set_mode(&src.join("b"), 0o400);
        set_mode(&src.join("x"), 0o400);
        fs::write(temp.path().join("list"), b"b\nx\nc\n").expect("write list");
        let shim = temp.path().join("rsh.sh");
        fs::write(&shim, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh shim");
        set_mode(&shim, 0o755);
        Self { temp }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    /// Copies `s/` into a fresh `d/`, locally or pushed with `binary` on both
    /// ends of the shim.
    fn run(&self, binary: &Path, push: bool, opts: &[&str]) -> (i32, String) {
        let dest = self.path("d");
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir(&dest).expect("mkdir d");
        let mut command = Command::new(binary);
        command.current_dir(self.temp.path());
        if push {
            command
                .arg("-e")
                .arg(self.path("rsh.sh"))
                .arg(format!("--rsync-path={}", binary.display()));
        }
        command.args(opts).arg("s/");
        if push {
            command.arg(format!("host:{}/", dest.display()));
        } else {
            command.arg("d/");
        }
        let output = run_with_timeout(command);
        let text = String::from_utf8_lossy(&output.stderr)
            .replace(&self.temp.path().display().to_string(), "W");
        (output.status.code().unwrap_or(-1), text)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for dir in ["s/b", "s/x", "d/b", "d/x"] {
            let path = self.path(dir);
            if path.exists() {
                set_mode(&path, 0o755);
            }
        }
    }
}

fn run_with_timeout(mut command: Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rsync");
    let deadline = Instant::now() + RUN_TIMEOUT;
    while child.try_wait().expect("try_wait").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("rsync did not exit within {RUN_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().expect("collect output")
}

/// The `rsync: [sender] ...` diagnostic lines, sorted: the order directories
/// are scanned in is the file-list order, which these cells do not pin.
fn sender_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|line| line.starts_with("rsync: [sender]"))
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

fn assert_cells(binary: &Path, label: &str) {
    let fixture = Fixture::new();
    for (cell, push) in CELLS.iter().flat_map(|cell| [(cell, false), (cell, true)]) {
        let (rc, text) = fixture.run(binary, push, cell.opts);
        let name = format!("{label} push={push} {:?}", cell.opts);
        assert_eq!(
            sender_lines(&text),
            cell.lines,
            "{name}: diagnostics; output:\n{text}"
        );
        assert_eq!(rc, cell.code, "{name}: exit code; output:\n{text}");
        if cell.opts.contains(&"-d") {
            continue;
        }
        assert_eq!(
            fs::read(fixture.path("d/c/g")).expect("readable sibling copied"),
            b"y\n",
            "{name}: the readable sibling must still land"
        );
    }
}

#[test]
fn scan_of_an_unsearchable_directory_matches_3_5_1() {
    if is_root() {
        eprintln!("skip: running as root, directory modes are not enforced");
        return;
    }
    assert_cells(&oc_rsync_binary(), "oc-rsync");
}

#[test]
fn upstream_oracle_agrees_with_the_pinned_cells() {
    if is_root() {
        eprintln!("skip: running as root, directory modes are not enforced");
        return;
    }
    let Some(upstream) =
        test_support::require_upstream_rsync(test_support::UpstreamVersion::V3_5_1)
    else {
        return;
    };
    assert_cells(upstream.binary(), "upstream 3.5.1");
}
