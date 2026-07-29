//! INC-S1a: oc-rsync sender vs a real upstream 3.4.4 receiver under
//! INC_RECURSE.
//!
//! oc<->oc runs cannot detect a symmetric incremental-recursion bug: both
//! ends share the same file-list model, so a malformed initial list is
//! produced and accepted by the same code. The "#84" family (a
//! `--files-from` entry whose implied parent is a symlink was dropped from
//! the list, and upstream's receiver aborted with "ABORTING due to invalid
//! path from sender", flist.c:2691, exit 4) lived exactly in this blind
//! spot. This oracle pins the sender-facing half: oc pushes `-r` to a real
//! upstream 3.4.4 receiver over both transports (remote-shell wrapper and
//! daemon) and the upstream binary is the judge.
//!
//! # Proving INC_RECURSE actually negotiated
//!
//! The literal client banner `receiving incremental file list`
//! (flist.c:2607) can never appear here: it is printed via `FCLIENT` under
//! an `!am_server` gate, and in both legs the upstream receiver runs as a
//! server. The oracle therefore asserts the server-side equivalents, which
//! are stronger because they count actual wire segments:
//!
//! - the capability string oc sends carries `i`
//!   (`-re.iLsfxCIvu`; upstream compat.c:162-181 `set_allow_inc_recurse()`,
//!   options.c:3036 `maybe_add_e_option()`) - captured from the wrapper's
//!   argv log on the remote-shell leg;
//! - with `--debug=flist2` forwarded to the receiver, upstream prints one
//!   `received %d names` line per `recv_file_list()` call (flist.c:2726)
//!   and one `[receiver] receiving flist for dir %d` line per extra
//!   sub-list (rsync.c:373). A non-INC_RECURSE transfer produces exactly
//!   one `received %d names` line and zero `receiving flist for dir`
//!   lines, so more than one segment is proof the receiver ran the
//!   incremental ladder. The negative control below pins that distinction
//!   so the segment-count oracle cannot rot into a vacuous pass.
//!
//! The fixture holds more than `MIN_FILECNT_LOOKAHEAD = 1000` files
//! (rsync.h:151) across three directory levels so the sender-side lookahead
//! throttle (sender.c:231,265 `send_extra_file_list()`) is exercised, not
//! just the segment framing.
//!
//! # Gating
//!
//! Mirrors `crates/transfer/tests/upstream_compat_daemon_gzip_goodbye.rs`:
//! skip silently unless `OC_RSYNC_UPSTREAM_COMPAT=1`; once enabled, a
//! missing upstream 3.4.4 binary fails loudly. `require_upstream_rsync`
//! verifies the `--version` banner, so macOS `/usr/bin/rsync` (openrsync)
//! can never be mistaken for upstream.

#![cfg(unix)]

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use tempfile::{TempDir, tempdir};
use test_support::{
    DirDiff, DirDiffOptions, UpstreamVersion, require_upstream_rsync, spawn_daemon_on_free_port,
    upstream_compat_enabled,
};

/// upstream: rsync.h:151 `#define MIN_FILECNT_LOOKAHEAD 1000`.
const MIN_FILECNT_LOOKAHEAD: usize = 1000;

/// Fixture shape: 12 top-level dirs x 2 subdirs each, 30 files per dir.
const TOP_DIRS: usize = 12;
const SUBDIRS_PER_TOP: usize = 2;
const FILES_PER_DIR: usize = 30;
/// 36 directories x 30 files = 1080 regular files.
const TOTAL_FILES: usize = TOP_DIRS * (1 + SUBDIRS_PER_TOP) * FILES_PER_DIR;

/// The binary under test, resolved at compile time so a stale on-disk
/// build can never be silently substituted.
fn oc_rsync_bin() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// Resolve the upstream 3.4.4 receiver, honoring the sibling gating
/// contract: `None` when the compat cell is not selected, loud panic when
/// it is selected but the binary is absent.
fn upstream_or_skip() -> Option<test_support::UpstreamRsync> {
    if !upstream_compat_enabled() {
        return None;
    }
    Some(require_upstream_rsync(UpstreamVersion::V3_4_4).expect(
        "OC_RSYNC_UPSTREAM_COMPAT=1 selected this test but upstream rsync 3.4.4 is not \
         installed; build it with `bash tools/ci/run_interop.sh` or point \
         OC_RSYNC_UPSTREAM_BIN_3_4_4 at it",
    ))
}

/// Build the >1000-file, three-level source tree. Every file's bytes
/// encode its own relative path so any cross-wiring of names to contents
/// fails the byte-equality walk.
fn build_large_tree(root: &Path) -> io::Result<()> {
    let mut written = 0usize;
    for top in 0..TOP_DIRS {
        let mut dirs = vec![root.join(format!("top{top:02}"))];
        for sub in 0..SUBDIRS_PER_TOP {
            dirs.push(root.join(format!("top{top:02}/sub{sub}")));
        }
        for dir in dirs {
            fs::create_dir_all(&dir)?;
            for file in 0..FILES_PER_DIR {
                let path = dir.join(format!("f{file:03}.txt"));
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                fs::write(&path, format!("{rel}\npayload {written}\n"))?;
                written += 1;
            }
        }
    }
    assert_eq!(
        written, TOTAL_FILES,
        "tree builder must match the constants"
    );
    assert!(
        written > MIN_FILECNT_LOOKAHEAD,
        "fixture must exceed the sender lookahead window to exercise throttling"
    );
    Ok(())
}

/// Write a remote-shell wrapper that appends its full argv to `argv_log`,
/// drops the hostname, and execs the rest. With `--rsync-path` pointing at
/// the upstream binary, the exec'd command IS the upstream server, so the
/// wrapper is a transparent transport plus an argv capture point.
fn write_rsh_wrapper(dir: &Path, argv_log: &Path) -> PathBuf {
    let wrapper = dir.join("rsh-wrapper.sh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nshift\nexec \"$@\"\n",
            argv_log.display()
        ),
    )
    .expect("write rsh wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod rsh wrapper");
    wrapper
}

/// Run the oc-rsync client and capture its output.
fn run_oc_client(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(oc_rsync_bin())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn oc-rsync client")
}

/// Count `received %d names` receiver lines (flist.c:2726, FLIST >= 2).
/// One per `recv_file_list()` call, so exactly 1 without INC_RECURSE.
fn count_received_names(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|l| l.starts_with("received ") && l.ends_with(" names"))
        .count()
}

/// Count `[receiver] receiving flist for dir %d` lines (rsync.c:373,
/// FLIST >= 2). These only exist on the INC_RECURSE extra-sub-list path.
fn count_flist_for_dir(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|l| l.contains("] receiving flist for dir "))
        .count()
}

/// Extract the capability characters after `e.` in the compact flag
/// argument of the captured server argv (e.g. `-re.iLsfxCIvu` -> `iLsfxCIvu`).
fn server_capabilities(argv_log: &Path) -> String {
    let argv = fs::read_to_string(argv_log).expect("read wrapper argv log");
    let flags = argv
        .lines()
        .find(|a| a.starts_with('-') && !a.starts_with("--"))
        .unwrap_or_else(|| panic!("no compact flag string in server argv:\n{argv}"));
    flags
        .split("e.")
        .nth(1)
        .unwrap_or_else(|| panic!("no capability suffix in server flags {flags}"))
        .to_owned()
}

/// Assert the destination tree is byte-identical to the source.
fn assert_trees_equal(src: &Path, dest: &Path) {
    let opts = DirDiffOptions {
        check_content: true,
        ..DirDiffOptions::default()
    };
    match DirDiff::compare(src, dest, opts).expect("tree walk") {
        Ok(()) => {}
        Err(mismatch) => panic!("{}", mismatch.into_panic_message()),
    }
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} must exit 0; status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Remote-shell leg: oc sender pushes the >1000-file tree to an upstream
/// receiver spawned through the wrapper. Asserts exit 0, byte equality,
/// the advertised `i` capability, and more than one received segment.
#[test]
fn rsh_push_negotiates_inc_recurse_and_segments_flist() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    build_large_tree(&src).expect("build source tree");
    fs::create_dir_all(&dest).expect("create dest");

    let argv_log = tmp.path().join("server-argv.log");
    let wrapper = write_rsh_wrapper(tmp.path(), &argv_log);

    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("fakehost:{}/", dest.display());
    let rsync_path = format!("--rsync-path={}", upstream.binary().display());
    let output = run_oc_client(
        &[
            "-r",
            "--debug=flist2",
            "--timeout=30",
            "--rsh",
            wrapper.to_str().expect("utf-8 wrapper path"),
            &rsync_path,
            &src_arg,
            &dest_arg,
        ]
        .map(std::ffi::OsStr::new),
    );
    assert_success(&output, "oc-rsync push via rsh wrapper to upstream 3.4.4");

    // Negotiation evidence, sender side: oc must advertise INC_RECURSE.
    // upstream: compat.c:162-181, options.c:3036 maybe_add_e_option().
    let caps = server_capabilities(&argv_log);
    assert!(
        caps.contains('i'),
        "oc sender must advertise the INC_RECURSE capability 'i' for a -r push; \
         a fallback to the whole-list protocol would silently defeat this oracle \
         (capabilities: e.{caps})"
    );

    // Negotiation evidence, receiver side: multiple flist segments.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let segments = count_received_names(&stdout);
    let extra_lists = count_flist_for_dir(&stdout);
    assert!(
        segments > 1,
        "upstream receiver must report more than one 'received N names' segment \
         under INC_RECURSE (flist.c:2726); got {segments}\nstdout:\n{stdout}"
    );
    assert!(
        extra_lists >= 1,
        "upstream receiver must report per-directory sub-lists \
         ('receiving flist for dir', rsync.c:373); got {extra_lists}\nstdout:\n{stdout}"
    );

    assert_trees_equal(&src, &dest);
}

/// Negative control: `--no-inc-recursive` must withhold `i` and collapse
/// the receiver to exactly one `received N names` segment. This pins the
/// discriminating power of the segment-count oracle - if this control ever
/// sees multiple segments, the positive assertions above prove nothing.
#[test]
fn rsh_push_no_inc_recursive_control_yields_single_segment() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    // A small multi-dir tree is enough: the control only needs directories
    // that WOULD segment under INC_RECURSE.
    for dir in ["a/b", "a/c", "d"] {
        let path = src.join(dir);
        fs::create_dir_all(&path).expect("control tree dir");
        fs::write(path.join("file.txt"), dir).expect("control tree file");
    }
    fs::create_dir_all(&dest).expect("create dest");

    let argv_log = tmp.path().join("server-argv.log");
    let wrapper = write_rsh_wrapper(tmp.path(), &argv_log);

    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("fakehost:{}/", dest.display());
    let rsync_path = format!("--rsync-path={}", upstream.binary().display());
    let output = run_oc_client(
        &[
            "-r",
            "--no-inc-recursive",
            "--debug=flist2",
            "--timeout=30",
            "--rsh",
            wrapper.to_str().expect("utf-8 wrapper path"),
            &rsync_path,
            &src_arg,
            &dest_arg,
        ]
        .map(std::ffi::OsStr::new),
    );
    assert_success(
        &output,
        "oc-rsync --no-inc-recursive push to upstream 3.4.4",
    );

    let caps = server_capabilities(&argv_log);
    assert!(
        !caps.contains('i'),
        "--no-inc-recursive must withhold the 'i' capability (capabilities: e.{caps})"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        count_received_names(&stdout),
        1,
        "a whole-list transfer produces exactly one 'received N names' line\nstdout:\n{stdout}"
    );
    assert_eq!(
        count_flist_for_dir(&stdout),
        0,
        "a whole-list transfer never receives per-dir sub-lists\nstdout:\n{stdout}"
    );

    assert_trees_equal(&src, &dest);
}

/// RAII kill-and-reap guard for the upstream daemon listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Daemon leg fixture: an upstream `rsync --daemon` exposing one
/// read-write module. `max verbosity = 4` lets the client-requested
/// `--debug=flist2` through the daemon's verbosity clamp
/// (upstream clientserver.c `lp_max_verbosity`).
struct UpstreamDaemon {
    _workdir: TempDir,
    module_root: PathBuf,
    port: u16,
    _daemon: DaemonGuard,
}

impl UpstreamDaemon {
    fn start(upstream: &test_support::UpstreamRsync) -> io::Result<Self> {
        let workdir = tempdir()?;
        let module_root = workdir.path().join("module");
        fs::create_dir_all(&module_root)?;
        let config_path = workdir.path().join("rsyncd.conf");
        fs::write(
            &config_path,
            format!(
                "pid file = {pid}\n\
                 log file = {log}\n\
                 use chroot = false\n\
                 munge symlinks = no\n\
                 max verbosity = 4\n\
                 \n\
                 [push]\n\
                 path = {module}\n\
                 read only = false\n",
                pid = workdir.path().join("rsyncd.pid").display(),
                log = workdir.path().join("rsyncd.log").display(),
                module = module_root.display(),
            ),
        )?;

        let binary = upstream.binary().to_path_buf();
        let (child, port) = spawn_daemon_on_free_port(|port| {
            Command::new(&binary)
                .arg("--daemon")
                .arg("--no-detach")
                .arg(format!("--port={port}"))
                .arg("--address=127.0.0.1")
                .arg(format!("--config={}", config_path.display()))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })?;

        Ok(Self {
            _workdir: workdir,
            module_root,
            port,
            _daemon: DaemonGuard { child },
        })
    }
}

/// Daemon leg: oc sender pushes the >1000-file tree into an upstream
/// daemon module. No argv capture exists on this transport, so the
/// receiver-side segment counts carry the whole negotiation proof.
#[test]
fn daemon_push_negotiates_inc_recurse_and_segments_flist() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };

    let daemon = UpstreamDaemon::start(&upstream).expect("start upstream rsync daemon");

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    build_large_tree(&src).expect("build source tree");

    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("rsync://127.0.0.1:{}/push/", daemon.port);
    let output = run_oc_client(
        &["-r", "--debug=flist2", "--timeout=30", &src_arg, &dest_arg].map(std::ffi::OsStr::new),
    );
    assert_success(&output, "oc-rsync push to upstream 3.4.4 daemon");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let segments = count_received_names(&stdout);
    let extra_lists = count_flist_for_dir(&stdout);
    assert!(
        segments > 1,
        "upstream daemon receiver must report more than one 'received N names' \
         segment under INC_RECURSE (flist.c:2726); got {segments}\nstdout:\n{stdout}"
    );
    assert!(
        extra_lists >= 1,
        "upstream daemon receiver must report per-directory sub-lists \
         ('receiving flist for dir', rsync.c:373); got {extra_lists}\nstdout:\n{stdout}"
    );

    assert_trees_equal(&src, &daemon.module_root);
}

/// Regression pin for the #84 shape: a `--files-from` entry whose implied
/// parent is a symlink. `send_implied_dirs()` runs with `copy_links` and
/// `xfer_dirs` forced on (flist.c:1983-1985), so upstream stats the parent
/// through the symlink and sends it as a directory. Before the fix, oc
/// used `symlink_metadata`, dropped the parent, and shipped `link/file`
/// with no `link` entry - upstream's receiver rejects that list with
/// "ABORTING due to invalid path from sender" (flist.c:2691) and exits 4.
/// An oc<->oc run cannot see this: only the real receiver aborts.
#[test]
fn files_from_symlinked_implied_parent_does_not_abort() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    fs::create_dir_all(src.join("realdir")).expect("create realdir");
    fs::write(src.join("realdir/file.txt"), b"payload-84\n").expect("write source file");
    std::os::unix::fs::symlink("realdir", src.join("link")).expect("create symlinked parent");
    fs::create_dir_all(&dest).expect("create dest");
    let list = tmp.path().join("list.txt");
    fs::write(&list, b"link/file.txt\n").expect("write files-from list");

    let argv_log = tmp.path().join("server-argv.log");
    let wrapper = write_rsh_wrapper(tmp.path(), &argv_log);

    let files_from = format!("--files-from={}", list.display());
    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("fakehost:{}/", dest.display());
    let rsync_path = format!("--rsync-path={}", upstream.binary().display());
    let output = run_oc_client(
        &[
            "-r",
            &files_from,
            "--timeout=30",
            "--rsh",
            wrapper.to_str().expect("utf-8 wrapper path"),
            &rsync_path,
            &src_arg,
            &dest_arg,
        ]
        .map(std::ffi::OsStr::new),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("ABORTING due to invalid path from sender"),
        "upstream receiver rejected the file list (the #84 regression signature):\n{stderr}"
    );
    assert_success(
        &output,
        "oc-rsync --files-from push with symlinked implied parent",
    );

    // The implied parent must arrive as a real directory (the symlink was
    // followed on the sender), holding the exact file bytes.
    let dest_link = dest.join("link");
    let meta = fs::symlink_metadata(&dest_link).expect("stat dest/link");
    assert!(
        meta.file_type().is_dir(),
        "implied parent must materialize as a real directory, not {:?}",
        meta.file_type()
    );
    let bytes = fs::read(dest_link.join("file.txt")).expect("read dest file");
    assert_eq!(bytes, b"payload-84\n", "file bytes must round-trip");
}
