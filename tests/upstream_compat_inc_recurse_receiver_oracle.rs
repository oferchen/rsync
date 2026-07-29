//! INC-S1b: a real upstream 3.4.4 SENDER vs an oc-rsync RECEIVER under
//! INC_RECURSE - the receiver-facing twin of INC-S1a
//! (`upstream_compat_inc_recurse_sender_oracle.rs`, #7036).
//!
//! An oc<->oc run cannot expose a receiver-side negotiation or segmentation
//! bug: both ends share the same file-list model, so whatever the oc server
//! writes for CF_INC_RECURSE, the oc client reads back and both agree. This
//! oracle pins the other half - a real upstream 3.4.4 sender pushes a
//! recursive tree INTO oc as the server-receiver over both real transports
//! (remote-shell wrapper and daemon) and the upstream binary is the judge.
//!
//! # What this oracle proves today (the baseline)
//!
//! oc's server-receiver never advertises CF_INC_RECURSE:
//! `compute_allow_inc_recurse` (`crates/transfer/src/lib.rs:428`) returns
//! `recursive && !qsort && role == ServerRole::Generator`, and a PUSH into
//! oc runs the Receiver role, so the bit is written 0 unconditionally
//! (`docs/design/receiver-inc-recurse-conversion.md` Section 3.2, merged as
//! #7050/#204). This test ENCODES that current non-negotiation as its
//! baseline so the RS chain (#205 flips the bit, #206 the RSS win, #207 the
//! wire capture) has a truthful regression floor. It does NOT change
//! production negotiation; when #205 lands, the negotiation assertion here
//! flips from "marker absent" to "marker present".
//!
//! # How the negotiation is observed
//!
//! The upstream SERVER is the sole author of CF_INC_RECURSE
//! (`// upstream: compat.c:597-598,713,739`); the CLIENT reads it back and
//! both derive `inc_recurse` (`// upstream: compat.c:746`). Here the pushing
//! CLIENT is the upstream sender we invoke, so its own output is the readout
//! of what oc-the-server negotiated:
//!
//! ```text
//! show_filelist_progress = INFO_GTE(FLIST,1) && xfer_dirs && !am_server && !inc_recurse
//!                                                       // upstream: flist.c:164
//! if (show_filelist_progress)
//!     start_filelist_progress("building file list");   // upstream: flist.c:2249
//! else if (inc_recurse && INFO_GTE(FLIST,1) && !am_server)
//!     rprintf(FCLIENT, "sending incremental file list\n");  // upstream: flist.c:2252
//! ```
//!
//! The two branches are mutually exclusive on `inc_recurse`. With a single
//! `-v` (which sets `INFO_FLIST = 1`, `// upstream: options.c:250
//! info_verbosity[1]`) the pushing sender prints the literal
//! `sending incremental file list` IF AND ONLY IF the receiving server
//! negotiated CF_INC_RECURSE. Against oc today the marker is ABSENT; the
//! positive control below drives the same client against a real upstream
//! server-receiver and asserts the marker PRESENT, so the absence in the oc
//! legs is discriminating, never a vacuous pass.
//!
//! On the remote-shell leg the wrapper also captures the compact `-e.` flags
//! the upstream client hands oc's `--server`: the client always advertises
//! the `i` capability for a `-r` push (`// upstream: compat.c:163-169`,
//! condition 4 of the receiver gate). Asserting `i` is present there proves
//! the client OFFERED incremental recursion and oc declined it - not that
//! the client simply never asked.
//!
//! The fixture holds more than `MIN_FILECNT_LOOKAHEAD = 1000` files
//! (`// upstream: rsync.h:151`) across three directory levels so that once
//! #205 flips the bit this same tree exercises real multi-segment reception,
//! not just the marker.
//!
//! # Gating
//!
//! Mirrors the sibling sender oracle: skip silently unless
//! `OC_RSYNC_UPSTREAM_COMPAT=1`; once enabled, a missing or mis-versioned
//! upstream 3.4.4 binary self-skips via `require_upstream_rsync`, whose
//! `--version` banner check keeps macOS `/usr/bin/rsync` (openrsync) from
//! ever standing in for upstream.

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

/// The literal the pushing upstream sender prints under `-v` only when the
/// receiving server negotiated CF_INC_RECURSE (upstream flist.c:2252).
const INC_MARKER: &str = "sending incremental file list";

/// Fixture shape: 12 top-level dirs x 2 subdirs each, 30 files per dir.
const TOP_DIRS: usize = 12;
const SUBDIRS_PER_TOP: usize = 2;
const FILES_PER_DIR: usize = 30;
/// 36 directories x 30 files = 1080 regular files.
const TOTAL_FILES: usize = TOP_DIRS * (1 + SUBDIRS_PER_TOP) * FILES_PER_DIR;

/// The binary under test, resolved at compile time so a stale on-disk build
/// can never be silently substituted. This is oc's server-receiver on both
/// legs (spawned as `--daemon` or, via the wrapper, as `--server`).
fn oc_rsync_bin() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// Resolve the upstream 3.4.4 sender, honoring the sibling gating contract:
/// `None` when the compat cell is not selected, loud panic when it is
/// selected but the binary is absent.
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

/// Build the >1000-file, three-level source tree. Every file's bytes encode
/// its own relative path so any cross-wiring of names to contents fails the
/// byte-equality walk.
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
        "fixture must exceed the lookahead window so a later CF_INC_RECURSE=1 \
         run genuinely segments"
    );
    Ok(())
}

/// Write a remote-shell wrapper that appends its full argv to `argv_log`,
/// drops the hostname, and execs the rest. With `--rsync-path` pointing at
/// the receiving binary, the exec'd command IS that server, so the wrapper
/// is a transparent transport plus an argv capture point.
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

/// Run an upstream rsync client push and capture its output. `receiver_path`
/// is the `--rsync-path` binary the wrapper execs as the server-receiver
/// (oc for the baseline legs, upstream for the positive control).
fn run_upstream_push_via_rsh(
    upstream: &test_support::UpstreamRsync,
    wrapper: &Path,
    receiver_path: &Path,
    src: &Path,
    dest: &Path,
) -> Output {
    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("fakehost:{}/", dest.display());
    let rsync_path = format!("--rsync-path={}", receiver_path.display());
    upstream
        .command()
        .args([
            "-r",
            "-v",
            "--timeout=30",
            "--rsh",
            wrapper.to_str().expect("utf-8 wrapper path"),
            &rsync_path,
            &src_arg,
            &dest_arg,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn upstream rsync push client")
}

/// Combined stdout+stderr of a client run, for marker scanning. FCLIENT
/// prints to the client's own stdout, but scanning both is robust against
/// stream routing.
fn combined_output(output: &Output) -> String {
    let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&output.stderr));
    s
}

/// Extract the capability characters after `e.` in the compact flag argument
/// of the captured server argv (e.g. `-re.iLsfxCIvu` -> `iLsfxCIvu`).
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

/// Remote-shell baseline: a real upstream 3.4.4 sender pushes the
/// 1080-file tree into oc as the ssh-style server-receiver. Asserts the
/// transfer succeeds, the tree is byte-identical, the client OFFERED the `i`
/// capability, and oc DECLINED it (no incremental marker) - the current
/// non-negotiation baseline.
#[test]
fn rsh_push_into_oc_receiver_does_not_negotiate_inc_recurse() {
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

    let output = run_upstream_push_via_rsh(&upstream, &wrapper, &oc_rsync_bin(), &src, &dest);
    assert_success(
        &output,
        "upstream 3.4.4 push via rsh wrapper into oc receiver",
    );

    // The upstream client offered incremental recursion: a `-r` push always
    // advertises the `i` capability (upstream compat.c:163-169, receiver gate
    // condition 4). So a non-incremental transfer proves oc DECLINED, not
    // that the client never asked.
    let caps = server_capabilities(&argv_log);
    assert!(
        caps.contains('i'),
        "upstream client must advertise the INC_RECURSE capability 'i' for a -r push \
         (capabilities: e.{caps})"
    );

    // Baseline: oc's server-receiver writes CF_INC_RECURSE = 0
    // (compute_allow_inc_recurse, crates/transfer/src/lib.rs:428, Receiver
    // role), so the pushing sender never prints the incremental marker.
    // When #205 flips the bit, this assertion flips to `assert!(contains)`.
    let out = combined_output(&output);
    assert!(
        !out.contains(INC_MARKER),
        "oc receiver must NOT negotiate CF_INC_RECURSE today, yet the upstream sender \
         printed {INC_MARKER:?} (flist.c:2252) - the baseline changed, see #205\n{out}"
    );

    assert_trees_equal(&src, &dest);
}

/// Positive control (discriminating power): the SAME upstream client pushes
/// into a real upstream 3.4.4 server-receiver, which DOES negotiate
/// CF_INC_RECURSE, so the marker MUST appear. If this control ever loses the
/// marker, the baseline's "marker absent" assertion proves nothing.
#[test]
fn rsh_push_into_upstream_receiver_negotiates_inc_recurse_control() {
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

    let output = run_upstream_push_via_rsh(&upstream, &wrapper, upstream.binary(), &src, &dest);
    assert_success(
        &output,
        "upstream 3.4.4 push via rsh wrapper into upstream 3.4.4 receiver",
    );

    let out = combined_output(&output);
    assert!(
        out.contains(INC_MARKER),
        "an upstream server-receiver negotiates CF_INC_RECURSE, so the pushing sender \
         must print {INC_MARKER:?} (flist.c:2252); its absence would void the oc \
         baseline's discriminating power\n{out}"
    );

    assert_trees_equal(&src, &dest);
}

/// RAII kill-and-reap guard for a spawned daemon listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Daemon leg fixture: `oc-rsync --daemon` exposing one read-write module,
/// so an upstream sender can push into it.
struct OcDaemon {
    _workdir: TempDir,
    module_root: PathBuf,
    port: u16,
    _daemon: DaemonGuard,
}

impl OcDaemon {
    fn start() -> io::Result<Self> {
        let workdir = tempdir()?;
        let module_root = workdir.path().join("module");
        fs::create_dir_all(&module_root)?;
        let config_path = workdir.path().join("oc-rsyncd.conf");
        fs::write(
            &config_path,
            format!(
                "use chroot = no\n\
                 \n\
                 [push]\n\
                 path = {module}\n\
                 read only = false\n",
                module = module_root.display(),
            ),
        )?;

        let binary = oc_rsync_bin();
        let (child, port) = spawn_daemon_on_free_port(|port| {
            Command::new(&binary)
                .arg("--daemon")
                .arg("--no-detach")
                .arg(format!("--port={port}"))
                .arg("--address=127.0.0.1")
                .arg("--config")
                .arg(&config_path)
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

/// Daemon baseline: a real upstream 3.4.4 sender pushes the >1000-file tree
/// into an oc daemon module. No argv capture exists on this transport, so
/// the client's incremental marker carries the whole negotiation proof.
#[test]
fn daemon_push_into_oc_receiver_does_not_negotiate_inc_recurse() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };

    let daemon = OcDaemon::start().expect("start oc-rsync daemon");

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    build_large_tree(&src).expect("build source tree");

    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("rsync://127.0.0.1:{}/push/", daemon.port);
    let output = upstream
        .command()
        .args(["-r", "-v", "--timeout=30", &src_arg, &dest_arg])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn upstream rsync daemon push");
    assert_success(&output, "upstream 3.4.4 push into oc daemon");

    // Baseline: an oc daemon receiver writes CF_INC_RECURSE = 0, so the
    // upstream sender never prints the incremental marker. This is the path
    // #102/#205 will flip; the assertion is what proves the change then.
    let out = combined_output(&output);
    assert!(
        !out.contains(INC_MARKER),
        "oc daemon receiver must NOT negotiate CF_INC_RECURSE today, yet the upstream \
         sender printed {INC_MARKER:?} (flist.c:2252) - baseline changed, see #205\n{out}"
    );

    assert_trees_equal(&src, &daemon.module_root);
}
