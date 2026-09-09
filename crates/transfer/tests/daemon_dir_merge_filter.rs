//! A daemon module's `filter = : NAME` must be read per directory.
//!
//! # What this pins
//!
//! `mergelist_parents` is a GLOBAL registry: `add_rule` files every
//! `FILTRULE_PERDIR_MERGE` rule into it (`exclude.c:349-391`) whatever list the
//! rule itself went into, so a rule that came from a module's `filter`
//! directive - and therefore lives in `daemon_filter_list`
//! (`clientserver.c:934`) - is registered alongside the transfer's own. The
//! sender walk then calls `change_local_filter_dir` -> `push_local_filters`
//! per directory (`flist.c:2265-2331`), which populates that rule's
//! `u.mergelist`, and `check_filter` recurses into `ent->u.mergelist` for every
//! PERDIR_MERGE entry it walks. The daemon list therefore DOES descend per
//! directory.
//!
//! ⚠ The load-bearing assertion is that `sub/bait.txt` is HIDDEN. Nothing names
//! `bait.txt` in the daemon config: the only place that name appears is inside
//! `sub/.rsync-filter`. So the file can only disappear if the daemon actually
//! opened and read that file while walking `sub/`. A parser that produced an
//! ordinary exclude - whether of `.rsync-filter` or of the whole literal token
//! `: .rsync-filter` - cannot hide it, which is what makes this cell able to
//! tell a real per-directory merge from a mangled exclude rather than merely
//! observing that some rule was created.
//!
//! `sub/keep.txt` is the in-tree companion control: an over-refusing merge
//! reader that hid everything under `sub/` would satisfy the bait assertion on
//! its own, so both halves are asserted together.
//!
//! MEASURED against a real rsync 3.5.0 daemon (loopback TCP, `lsof`
//! port-ownership assert per cell), which is the behaviour asserted here:
//!
//! ```text
//! filter = : .rsync-filter          pull -> root.txt, sub/.rsync-filter, sub/keep.txt
//! filter = dir-merge .rsync-filter  pull -> identical
//! (no filter directive)             pull -> the same plus sub/bait.txt
//! ```
//!
//! # Upstream References
//!
//! - `exclude.c:1331-1338` - `case ':'` sets `FILTRULE_PERDIR_MERGE` and falls
//!   through to `case '.'` for `FILTRULE_MERGE_FILE`
//! - `exclude.c:1310-1311` - the `dir-merge` keyword maps onto `:`
//! - `exclude.c:349-391` - `add_rule` registers the rule in `mergelist_parents`
//! - `exclude.c:858-1000` - `push_local_filters` / `change_local_filter_dir`
//! - `clientserver.c:934` - `filter =` parses into `daemon_filter_list`

#![cfg(unix)]

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use tempfile::{TempDir, tempdir};

/// A running daemon with both pipes drained on their own threads.
struct Daemon {
    child: Child,
    port: u16,
    _stderr_rx: Receiver<String>,
    _stdout_rx: Receiver<String>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<String> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

fn spawn_daemon(bin: &Path, config_path: &Path) -> io::Result<Daemon> {
    let (mut child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(bin)
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
    let _stdout_rx = drain::<ChildStdout>(child.stdout.take());
    let _stderr_rx = drain::<ChildStderr>(child.stderr.take());
    Ok(Daemon {
        child,
        port,
        _stderr_rx,
        _stdout_rx,
    })
}

/// A module tree whose ONLY mention of `bait.txt` is inside `sub/.rsync-filter`.
struct Scratch {
    _tmp: TempDir,
    config: PathBuf,
    dest: PathBuf,
}

/// `directive` is written verbatim into the module, or omitted entirely when
/// `None` - the no-filter control.
fn scratch(directive: Option<&str>) -> io::Result<Scratch> {
    let tmp = tempdir()?;
    let root = tmp.path().to_path_buf();
    let module = root.join("module");
    let sub = module.join("sub");
    fs::create_dir_all(&sub)?;
    fs::write(module.join("root.txt"), b"root\n")?;
    fs::write(sub.join(".rsync-filter"), b"- bait.txt\n")?;
    fs::write(sub.join("bait.txt"), b"bait\n")?;
    fs::write(sub.join("keep.txt"), b"keep\n")?;

    let filter_line = match directive {
        Some(rule) => format!("    filter = {rule}\n"),
        None => String::new(),
    };
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "pid file = {pid}\n\
             log file = {log}\n\
             use chroot = false\n\
             \n\
             [m]\n\
             \x20   path = {module}\n\
             \x20   read only = false\n\
             \x20   list = true\n\
             {filter_line}",
            pid = root.join("rsyncd.pid").display(),
            log = root.join("rsyncd.log").display(),
            module = module.display(),
        ),
    )?;
    Ok(Scratch {
        _tmp: tmp,
        config,
        dest: root.join("dest"),
    })
}

/// Pulls the whole module and returns the relative paths that arrived, sorted.
fn pull_served_names(bin: &Path, scratch: &Scratch) -> io::Result<Vec<String>> {
    let daemon = spawn_daemon(bin, &scratch.config)?;
    let src = format!("rsync://127.0.0.1:{}/m/", daemon.port);
    let dest = format!("{}/", scratch.dest.display());
    let output = Command::new(bin)
        .args(["-r", &src, &dest])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "pull must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut names = Vec::new();
    let mut stack = vec![scratch.dest.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(&scratch.dest)
                    .expect("walked path is under dest");
                names.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// The control: with no `filter` directive the bait is served, so the fixture
/// can never pass by having planted an unreadable or absent bait file.
#[test]
fn without_a_filter_directive_the_bait_is_served() {
    let bin = test_support::oc_rsync_bin();
    let scratch = scratch(None).expect("scratch");
    let names = pull_served_names(&bin, &scratch).expect("pull");
    assert!(
        names.contains(&"sub/bait.txt".to_string()),
        "control must serve the bait, got {names:?}"
    );
    assert!(names.contains(&"sub/keep.txt".to_string()), "{names:?}");
}

/// Both spellings of the directive hide the bait and keep the control file.
#[test]
fn a_daemon_dir_merge_hides_a_name_only_the_merge_file_names() {
    let bin = test_support::oc_rsync_bin();
    for directive in [": .rsync-filter", "dir-merge .rsync-filter"] {
        let scratch = scratch(Some(directive)).expect("scratch");
        let names = pull_served_names(&bin, &scratch).expect("pull");
        assert!(
            !names.contains(&"sub/bait.txt".to_string()),
            "`filter = {directive}` must read sub/.rsync-filter and hide the \
             bait; served {names:?}"
        );
        assert!(
            names.contains(&"sub/keep.txt".to_string()),
            "`filter = {directive}` must not over-refuse the sibling control; \
             served {names:?}"
        );
        assert!(
            names.contains(&"root.txt".to_string()),
            "`filter = {directive}` must not over-refuse the module root; \
             served {names:?}"
        );
    }
}
