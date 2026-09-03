//! A daemon-served operand ending in `/.` must keep upstream's DOTDIR marker.
//!
//! # Background
//!
//! Upstream's daemon runs every client positional through
//! `sanitize_path(NULL, argv[i], "", 0, SP_KEEP_DOT_DIRS)` (`options.c:2402-2405`,
//! gated on the `sanitize_paths` that `clientserver.c:1068` sets for every
//! connection with a module dir). That sanitizer does NOT split the path into
//! components and rejoin them: it copies each component *through the next
//! slash* (`util1.c:1201`) and only then examines the following one, so a
//! discarded `.` (`util1.c:1163-1172`) or `..` (`util1.c:1183-1191`) leaves the
//! separator that preceded it in the output buffer. `sym-to-dir/.` therefore
//! sanitizes to `sym-to-dir/`, not to `sym-to-dir`.
//!
//! That surviving slash is load-bearing. `send_file_list()` reads it at
//! `flist.c:2589-2594` and sets `name_type = DOTDIR_NAME`, and the operand's
//! stat is then taken as
//! `link_stat(fbuf, &st, copy_dirlinks || name_type != NORMAL_NAME)`
//! (`flist.c:2696`). The second disjunct is the marker: with it, a symlink whose
//! target is a directory is followed and the directory's CONTENTS are sent
//! (`flist.c:286-299`); without it the operand is `lstat`ed and ships as a
//! symlink under its own basename.
//!
//! oc-rsync's daemon resolved the client tail by splitting on `/`, dropping
//! every `.`/`..` segment and rejoining the survivors, then re-attached a
//! trailing slash only when the RAW tail ended in one. `sym-to-dir/.` does not,
//! so the marker was destroyed before the sender ever saw the operand.
//!
//! # Why this matters (Rule 9)
//!
//! `rsync -a rsync://host/mod/current/. DEST` where `current -> releases/2026-08-25`
//! is an ordinary release-pointer pull. MEASURED on a loopback daemon against
//! real rsync 3.5.0: upstream delivers the release directory's contents; oc
//! delivered a single dangling symlink named `current` and exited 0.
//!
//! # Cells
//!
//! Each defect gets one discriminating cell and one non-vacuity companion. A
//! companion is a cell whose verdict does not depend on the fix, so a green
//! companion beside a red pin proves the fixture is live rather than inert.
//!
//! - `dotdir_marker_on_a_trailing_dot_follows_the_symlinked_directory` - the pin
//!   for the dropped marker.
//! - `an_operand_without_the_marker_still_ships_the_symlink_itself` - its
//!   companion. It asserts the OPPOSITE outcome for the marker-less spelling, so
//!   it fails if the symlink is missing, unreachable or already followed, and it
//!   also refuses a "fix" that simply follows every operand.
//! - `a_bare_dot_operand_transfers_the_whole_module` - the pin for the filed
//!   claim that a `.` operand is stripped and nothing transfers.
//! - `a_bare_module_root_operand_transfers_the_whole_module` - its companion:
//!   the same payload requested with the spelling that never carried a `.`, so
//!   it stays green whatever happens to dot handling and fails only if the
//!   module itself is unservable.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - the fixture plants a symlink and the daemon is spawned with
//! `use chroot = false`, matching the sibling daemon-spawning tests.
//!
//! # Skip semantics
//!
//! Self-skips (prints `skipping:` and returns) when a tempdir cannot be made or
//! the daemon fails to start. Any other divergence is a real regression.
//!
//! # Upstream References
//!
//! - `rsync-3.5.0/options.c:2402-2405` - the daemon's per-arg `sanitize_path()`
//! - `rsync-3.5.0/clientserver.c:1068` - `if (module_dirlen) sanitize_paths = 1`
//! - `rsync-3.5.0/util1.c:1163-1172` - a `.` component is skipped
//! - `rsync-3.5.0/util1.c:1201` - each component is copied *through* its slash
//! - `rsync-3.5.0/flist.c:2589-2594` - a trailing `/` sets `DOTDIR_NAME`
//! - `rsync-3.5.0/flist.c:2696` - `copy_dirlinks || name_type != NORMAL_NAME`
//! - `rsync-3.5.0/flist.c:286-299` - `link_stat()`'s `follow_dirlinks` arm

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::{TempDir, tempdir};

/// Payload inside the symlinked directory. Reaching it is the proof the link
/// was followed; a status-only assertion cannot see the difference.
const PAYLOAD: &[u8] = b"dotdir-marker payload\n";

const MODULE: &str = "dotmod";

fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_root: &Path,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [{MODULE}]\n\
         path = {root}\n\
         comment = dotdir operand marker regression\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        root = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Kills the daemon child on drop so a panicking cell never leaks the listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_oc_daemon(oc_bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(oc_bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    Ok((DaemonGuard { child }, port))
}

/// The module tree every cell serves:
///
/// ```text
/// <module>/realdir/f.txt
/// <module>/sym-to-dir -> realdir
/// ```
struct Fixture {
    _tmp: TempDir,
    config: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Option<Self> {
        let tmp = tempdir().ok()?;
        // macOS resolves `/tmp -> /private/tmp`; canonicalise before planting
        // the fixture's own symlink so the ambient prefix is not what is
        // measured.
        let root = fs::canonicalize(tmp.path()).ok()?;
        let module_root = root.join("module");
        let realdir = module_root.join("realdir");
        fs::create_dir_all(&realdir).ok()?;
        fs::write(realdir.join("f.txt"), PAYLOAD).ok()?;
        symlink("realdir", module_root.join("sym-to-dir")).ok()?;
        let config = root.join("rsyncd.conf");
        write_daemon_config(
            &config,
            &root.join("rsyncd.pid"),
            &root.join("rsyncd.log"),
            &module_root,
        )
        .ok()?;
        Some(Self {
            config,
            root,
            _tmp: tmp,
        })
    }
}

/// Everything a cell needs to judge a pull: exit status, stderr, and the
/// destination tree as `/`-joined relative paths in sorted order.
struct Pulled {
    status: std::process::ExitStatus,
    stderr: String,
    tree: Vec<String>,
}

/// Pulls `rsync://127.0.0.1:<port>/dotmod/<tail>` into a fresh destination.
fn pull(tail: &str) -> Option<Pulled> {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(fixture) = Fixture::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return None;
    };
    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &fixture.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return None;
        }
    };

    let dest = fixture.root.join("dest");
    fs::create_dir_all(&dest).expect("create destination");

    let src_url = OsString::from(format!("rsync://127.0.0.1:{port}/{MODULE}/{tail}"));
    let mut dest_arg = dest.clone().into_os_string();
    dest_arg.push("/");
    let args: &[&OsStr] = &[OsStr::new("-a"), &src_url, &dest_arg];

    let output = Command::new(&oc_bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn oc-rsync client (daemon pull)");

    Some(Pulled {
        status: output.status,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        tree: collect_tree(&dest, &dest),
    })
}

/// Relative `/`-joined names of everything under `dir`, sorted. Symlinks are
/// listed but never descended, so a followed link and a shipped link produce
/// visibly different trees.
fn collect_tree(root: &Path, dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let is_dir = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir());
        out.push(rel);
        if is_dir {
            out.extend(collect_tree(root, &path));
        }
    }
    out.sort();
    out
}

/// The pin for defect A: the marker survives the daemon's path resolution, so
/// the symlinked directory is followed and its CONTENTS arrive.
#[test]
fn dotdir_marker_on_a_trailing_dot_follows_the_symlinked_directory() {
    let Some(pulled) = pull("sym-to-dir/.") else {
        return;
    };
    assert!(
        pulled.status.success(),
        "pull of `{MODULE}/sym-to-dir/.` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec!["f.txt".to_owned()],
        "a `/.` operand carries upstream's DOTDIR marker (flist.c:2589-2594), so \
         link_stat follows the symlinked directory (flist.c:2696) and the \
         CONTENTS arrive; a tree containing `sym-to-dir` means the marker was \
         dropped and the operand was lstat'ed",
    );
}

/// Non-vacuity companion for defect A. Its verdict does not depend on the fix:
/// it fails if the fixture's symlink is missing or unreachable, and it fails if
/// a "fix" followed every operand instead of only marked ones.
#[test]
fn an_operand_without_the_marker_still_ships_the_symlink_itself() {
    let Some(pulled) = pull("sym-to-dir") else {
        return;
    };
    assert!(
        pulled.status.success(),
        "pull of `{MODULE}/sym-to-dir` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec!["sym-to-dir".to_owned()],
        "without the DOTDIR marker `link_stat`'s follow_dirlinks argument is \
         `copy_dirlinks` alone (flist.c:2696), which is off here, so the operand \
         must ship as the symlink it is",
    );
}

/// The pin for defect B as filed: a bare `.` operand must not be stripped.
#[test]
fn a_bare_dot_operand_transfers_the_whole_module() {
    let Some(pulled) = pull(".") else {
        return;
    };
    assert!(
        pulled.status.success(),
        "pull of `{MODULE}/.` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec![
            "realdir".to_owned(),
            "realdir/f.txt".to_owned(),
            "sym-to-dir".to_owned(),
        ],
        "a lone `.` operand names the module root with the DOTDIR marker \
         (flist.c:2601-2604), so the module's whole contents transfer; an empty \
         tree is the `.`-stripped-to-nothing shape",
    );
}

/// Non-vacuity companion for defect B: the same payload requested with the
/// spelling that never carried a `.`. It stays green whatever happens to dot
/// handling, so a red pin beside it means dot handling, not an unservable
/// module.
#[test]
fn a_bare_module_root_operand_transfers_the_whole_module() {
    let Some(pulled) = pull("") else {
        return;
    };
    assert!(
        pulled.status.success(),
        "pull of `{MODULE}/` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec![
            "realdir".to_owned(),
            "realdir/f.txt".to_owned(),
            "sym-to-dir".to_owned(),
        ],
        "the module root spelled without any `.` must serve its whole tree",
    );
}
