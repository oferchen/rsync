//! A daemon `--relative` pull must transmit MODULE-relative names.
//!
//! Upstream's daemon sender never sees an absolute operand. `read_args()`
//! routes every post-`.` positional through `glob_expand_module()`, which
//! strips the `MODULE/` prefix the client sent, and `parse_arguments()` then
//! re-roots whatever is left at the module with `sanitize_path(NULL, argv[i],
//! "", 0, SP_KEEP_DOT_DIRS)`. By the time `send_file_list()` splits the
//! positional, `curr_dir` is the module root the server `chdir()`ed into and
//! the transmitted name is the module-relative tail.
//!
//! oc-rsync never `chdir()`s: the daemon resolves each positional to an
//! absolute on-disk path and the sender walks that. Under `--relative` the
//! whole path is the transmitted name, so without re-supplying the module root
//! as the walk base the daemon's own filesystem prefix goes out on the wire.
//! Measured against rsync 3.5.0 serving the same fixture, every one of sixteen
//! operand shapes diverged: the receiver either refused the file list
//! (`rejecting unrequested file-list name`, flist.c:1144) or materialised the
//! server's absolute path under the destination.
//!
//! Ground truth for the two pinned shapes, captured from rsync 3.5.0 serving
//! module `mod` rooted at a tree of `top.txt`, `a/c.txt`, `a/b/file.txt`:
//!
//! ```text
//! rsync -R -r rsync://host/mod/a/b/file.txt dst/  ->  a, a/b, a/b/file.txt
//! rsync -R -r rsync://host/mod/            dst/  ->  a, a/b, a/b/file.txt, a/c.txt, top.txt
//! ```
//!
//! # The `/./` pivot
//!
//! Re-anchoring the walk base closed fourteen of the sixteen shapes. The two
//! that survived both carry an INTERIOR `/./`, and they diverged for a second,
//! independent reason: the daemon's own argv sanitize dropped the `.`
//! component before the sender could split on it.
//!
//! Upstream keeps that decision on ONE axis. `options.c:2405` sanitizes every
//! daemon positional with `SP_KEEP_DOT_DIRS`, and `util1.c:1143` reduces the
//! flag to `drop_dot_dirs = !relative_paths || !(flags & SP_KEEP_DOT_DIRS)` -
//! so on this path the surviving condition is `!relative_paths`. Under
//! `--relative` the `.` therefore reaches `flist.c:2623`'s
//! `strstr(fbuf, "/./")`, which splits the operand into the `dir` the sender
//! walks from and the `fn` it transmits. oc hard-coded the drop, which made
//! the axis "daemon-ness" instead of `--relative`: the pivot was erased, the
//! sender transmitted the pre-pivot prefix too, and the receiver refused the
//! list with `rejecting unrequested file-list name` (flist.c:1145, exit 4).
//!
//! Ground truth for the pivot shapes, captured from the same 3.5.0 daemon:
//!
//! ```text
//! rsync -R -r rsync://host/mod/a/./b/file.txt dst/  ->  b, b/file.txt
//! rsync -R -r rsync://host/mod/a/b/./file.txt dst/  ->  file.txt
//! rsync    -r rsync://host/mod/a/./b/file.txt dst/  ->  file.txt
//! ```
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/clientserver.c:1059` - `change_dir(module_chdir, CD_NORMAL)`
//! - `rsync-3.5.0/util1.c:881` - `glob_expand_module()` strips `MODULE/`
//! - `rsync-3.5.0/options.c:2405` - `sanitize_path(NULL, argv[i], "", 0, ..)`
//! - `rsync-3.5.0/flist.c:2610-2660` - the per-positional `dir`/`fn` split
//! - `rsync-3.5.0/flist.c:1144` - `rejecting unrequested file-list name`
//! - `rsync-3.5.0/util1.c:1143` - `drop_dot_dirs = !relative_paths || ..`
//! - `rsync-3.5.0/flist.c:2623` - `if ((p = strstr(fbuf, "/./")) != NULL)`

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::{TempDir, tempdir};

const MODULE: &str = "relmod";

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
         comment = relative daemon pull names\n\
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
/// <module>/top.txt
/// <module>/a/c.txt
/// <module>/a/b/file.txt
/// ```
struct Fixture {
    _tmp: TempDir,
    config: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Option<Self> {
        let tmp = tempdir().ok()?;
        // macOS resolves `/tmp -> /private/tmp`; canonicalise so the ambient
        // prefix is not what the tree assertion measures.
        let root = fs::canonicalize(tmp.path()).ok()?;
        let module_root = root.join("module");
        fs::create_dir_all(module_root.join("a/b")).ok()?;
        fs::write(module_root.join("top.txt"), b"top\n").ok()?;
        fs::write(module_root.join("a/c.txt"), b"c\n").ok()?;
        fs::write(module_root.join("a/b/file.txt"), b"f\n").ok()?;
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

/// Pulls `rsync://127.0.0.1:<port>/relmod/<tail>` into a fresh destination,
/// passing `extra` ahead of the source (`-R` for the pinned cells, nothing for
/// the negative control).
fn pull(tail: &str, extra: &[&str]) -> Option<Pulled> {
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
    let mut args: Vec<&OsStr> = vec![OsStr::new("-r")];
    args.extend(extra.iter().map(OsStr::new));
    args.push(&src_url);
    args.push(&dest_arg);

    let output = Command::new(&oc_bin)
        .args(&args)
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

/// Relative `/`-joined names of everything under `dir`, sorted.
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

/// A `--relative` pull of a sub-path names it from the module root.
///
/// Before the walk base was re-anchored, the sender transmitted the daemon's
/// absolute path, and the receiver's `rejecting unrequested file-list name`
/// check (flist.c:1144) refused the whole list - so this cell fails on the
/// status line, promptly, rather than hanging.
#[test]
fn relative_pull_of_a_subpath_names_it_from_the_module_root() {
    let Some(pulled) = pull("a/b/file.txt", &["-R"]) else {
        return;
    };
    assert!(
        pulled.status.success(),
        "`-R` pull of `{MODULE}/a/b/file.txt` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec!["a".to_owned(), "a/b".to_owned(), "a/b/file.txt".to_owned(),],
        "upstream's daemon sender names the positional against `curr_dir`, \
         which `clientserver.c:1059` put at the module root, so `-R` transmits \
         `a/b/file.txt`; any component of the server's own path in this tree \
         means the walk base was `/` instead of the module root",
    );
}

/// The whole-module operand takes the same base.
///
/// This shape TRANSFERS either way, so only the tree can see the defect: with
/// the walk base at `/` the client materialises the daemon's absolute path
/// under the destination instead of the module contents.
#[test]
fn relative_pull_of_the_module_root_names_its_contents_from_the_module_root() {
    let Some(pulled) = pull("", &["-R"]) else {
        return;
    };
    assert!(
        pulled.status.success(),
        "`-R` pull of `{MODULE}/` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec![
            "a".to_owned(),
            "a/b".to_owned(),
            "a/b/file.txt".to_owned(),
            "a/c.txt".to_owned(),
            "top.txt".to_owned(),
        ],
        "the module root operand resolves to the module root itself, so the \
         relative names are the module's own contents; a `private`/`tmp`/... \
         prefix here is the server's filesystem path on the wire",
    );
}

/// Negative control: the non-`--relative` path is a different upstream branch
/// (`flist.c:2610-2620` splits on the LAST `/`) and must be unaffected. It is
/// green before and after the re-anchoring, so a red pin beside a green
/// control proves the change is scoped to `--relative`.
#[test]
fn a_non_relative_pull_of_the_same_subpath_is_unchanged() {
    let Some(pulled) = pull("a/b/file.txt", &[]) else {
        return;
    };
    assert!(
        pulled.status.success(),
        "pull of `{MODULE}/a/b/file.txt` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec!["file.txt".to_owned()],
        "without `--relative` upstream sends the basename only",
    );
}

/// An interior `/./` pivots the transmitted name: everything before it is the
/// directory the sender walks from, and only the tail rides the wire.
///
/// upstream: `flist.c:2623-2634` splits `a/./b/file.txt` into `dir = "a"` and
/// `fn = "b/file.txt"`. The split can only happen if the `.` survived the
/// daemon's `options.c:2405` sanitize, which it does exactly when
/// `relative_paths` is on (`util1.c:1143`).
///
/// With the `.` dropped the sender transmitted `a/b/file.txt`, and the
/// receiver refused the unrequested `a` (`flist.c:1145`), so this cell fails
/// on the status line rather than hanging.
#[test]
fn relative_pull_pivots_the_name_at_an_interior_dot_dir() {
    let Some(pulled) = pull("a/./b/file.txt", &["-R"]) else {
        return;
    };
    assert!(
        pulled.status.success(),
        "`-R` pull of `{MODULE}/a/./b/file.txt` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec!["b".to_owned(), "b/file.txt".to_owned()],
        "the `/./` names `a` as the sender's directory, so only `b/file.txt` \
         rides the wire; an `a/` component here means the pivot was erased \
         before `flist.c:2623` could split on it",
    );
}

/// The pivot immediately before the leaf leaves the bare basename on the wire.
///
/// upstream: `a/b/./file.txt` splits into `dir = "a/b"` and `fn = "file.txt"`.
#[test]
fn relative_pull_pivots_at_the_leafs_parent() {
    let Some(pulled) = pull("a/b/./file.txt", &["-R"]) else {
        return;
    };
    assert!(
        pulled.status.success(),
        "`-R` pull of `{MODULE}/a/b/./file.txt` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec!["file.txt".to_owned()],
        "with the pivot at `a/b`, `--relative` transmits only `file.txt`",
    );
}

/// Negative control for the axis itself: the SAME pivot operand without
/// `--relative` must still lose its `.`.
///
/// upstream: `util1.c:1143` makes `drop_dot_dirs` true whenever
/// `relative_paths` is off, whatever `SP_KEEP_DOT_DIRS` says, and
/// `flist.c:2608` then splits on the LAST `/` - so the wire name is the bare
/// basename. A green pin beside the two red ones above proves the fix rides
/// `--relative` and not the daemon-ness of the process.
#[test]
fn a_non_relative_pull_of_a_pivot_operand_still_drops_the_dot_dir() {
    let Some(pulled) = pull("a/./b/file.txt", &[]) else {
        return;
    };
    assert!(
        pulled.status.success(),
        "pull of `{MODULE}/a/./b/file.txt` exited {:?}\nstderr:\n{}",
        pulled.status,
        pulled.stderr,
    );
    assert_eq!(
        pulled.tree,
        vec!["file.txt".to_owned()],
        "without `--relative` upstream sends the basename only",
    );
}

/// Concurrency cell: one daemon, two SIMULTANEOUS connections whose operands
/// pivot at different depths.
///
/// oc-rsync serves every daemon connection on a worker thread of one process
/// (`spawn_connection_worker`), not a forked child like upstream, so any
/// process-global carrying this decision - a `chdir()`, a `curr_dir`, a
/// static `relative_paths` - would let one connection's operand shape decide
/// the other's transmitted names. The fix threads the value as a parameter of
/// the per-connection resolvers, so there is nothing shared to race; this cell
/// is what says so out loud. It fails if the decision is ever hoisted to a
/// global: the two destinations would converge on one pivot.
#[test]
fn concurrent_connections_do_not_share_the_relative_pivot() {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(fixture) = Fixture::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };
    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &fixture.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    // Two shapes with DIFFERENT pivots and different expected trees, run at
    // the same time against the same daemon process.
    let cases: [(&str, Vec<String>); 2] = [
        (
            "a/./b/file.txt",
            vec!["b".to_owned(), "b/file.txt".to_owned()],
        ),
        ("a/b/./file.txt", vec!["file.txt".to_owned()]),
    ];

    let handles: Vec<_> = cases
        .into_iter()
        .enumerate()
        .map(|(i, (tail, expected))| {
            let oc_bin = oc_bin.clone();
            let dest = fixture.root.join(format!("cdest{i}"));
            fs::create_dir_all(&dest).expect("create destination");
            std::thread::spawn(move || {
                let src_url = OsString::from(format!("rsync://127.0.0.1:{port}/{MODULE}/{tail}"));
                let mut dest_arg = dest.clone().into_os_string();
                dest_arg.push("/");
                let output = Command::new(&oc_bin)
                    .args([
                        OsStr::new("-r"),
                        OsStr::new("-R"),
                        src_url.as_os_str(),
                        dest_arg.as_os_str(),
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .expect("spawn oc-rsync client (concurrent daemon pull)");
                (
                    tail,
                    expected,
                    output.status,
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                    collect_tree(&dest, &dest),
                )
            })
        })
        .collect();

    for handle in handles {
        let (tail, expected, status, stderr, tree) = handle.join().expect("client thread");
        assert!(
            status.success(),
            "concurrent `-R` pull of `{MODULE}/{tail}` exited {status:?}\nstderr:\n{stderr}",
        );
        assert_eq!(
            tree, expected,
            "concurrent `-R` pull of `{MODULE}/{tail}` transmitted the wrong \
             names; a shared pivot would make both connections agree on one",
        );
    }
}
