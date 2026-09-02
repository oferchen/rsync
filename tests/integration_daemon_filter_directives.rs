//! UTS-7.5: end-to-end regression for daemon `filter`/`exclude`/`include from`/
//! `exclude from` directives in rsyncd.conf.
//!
//! The wire-up funnels module config -> `build_daemon_filter_rules()` ->
//! `ServerConfig::daemon_filter_rules` -> the receiver's `daemon_filter_set`
//! plus a prepend into the deletion filter chain. This file pushes through a
//! real in-process daemon and asserts that patterns excluded by a module
//! directive never land on disk - regardless of whether the client supplied
//! any filter on its own.
//!
//! upstream: clientserver.c:934-951 - `rsync_module()` builds
//! `daemon_filter_list` from `filter` / `include_from` / `include` /
//! `exclude_from` / `exclude` in that order, then `check_filter()` at
//! `receiver.c:889` and `generator.c:1663` consults it before any
//! per-file action.
//!
//! A refused file is never dropped in silence: `generator.c:1669-1671` reports
//! it as `FERROR_XFER`, `log.c:337-338` sets `got_xfer_error` on receipt, and
//! `cleanup.c:217-218` lifts the push's exit status to `RERR_PARTIAL` (23,
//! `errcode.h:43`). Every test below asserts that status as well as the on-disk
//! end state, because the status is the signal that the module directive
//! actually fired: an absent `.log` file is equally consistent with a transfer
//! that achieved nothing at all.
//!
//! Gated `#[cfg(unix)]` because the in-process client + daemon split uses
//! POSIX TCP and the helper walks the destination with `read_dir`; the test
//! also skips silently when ephemeral port allocation fails (sandboxed CI),
//! mirroring the pattern from `integration_daemon_max_connections_cap.rs`
//! and the daemon crate's `daemon_itemize_push` chunk.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::sync::Mutex;
use std::thread;

use core::client::ClientConfig;
use daemon::{DaemonConfig, run_daemon};
use tempfile::tempdir;

/// Serialise daemon-spawning tests in this binary. Port allocation is
/// ephemeral but the lock keeps the source/destination tempdirs from
/// stepping on each other when nextest schedules multiple binaries
/// concurrently on a constrained CI runner.
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Upstream's exit status for a transfer that lost files to an error.
///
/// upstream: errcode.h:43 - `#define RERR_PARTIAL    23`.
const RERR_PARTIAL: i32 = 23;

/// Allocates a free TCP port for the test daemon. Returns both the port and
/// the bound `TcpListener` so the listener can be handed to the daemon via
/// `pre_bound_listener` - this closes the TOCTOU window between port
/// allocation and the daemon's own bind.
fn allocate_test_port() -> Option<(u16, TcpListener)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    Some((port, listener))
}

/// Walks `root` recursively and maps each relative file path to its contents.
///
/// The contents travel with the names so that a kept file has to arrive intact
/// to satisfy the positive assertions. Name-only presence would also be
/// satisfied by an empty placeholder left behind by a half-finished transfer.
fn collect_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                if let (Ok(rel), Ok(contents)) = (path.strip_prefix(root), fs::read(&path)) {
                    out.insert(rel.to_string_lossy().into_owned(), contents);
                }
            }
        }
    }
    out
}

/// Asserts that `name` landed under the destination carrying `expected`.
fn assert_kept(files: &BTreeMap<String, Vec<u8>>, name: &str, expected: &[u8]) {
    let actual = files.get(name).unwrap_or_else(|| {
        panic!(
            "{name} must be transferred, destination holds {:?}",
            files.keys().collect::<Vec<_>>()
        )
    });
    assert_eq!(
        actual.as_slice(),
        expected,
        "{name} was transferred with the wrong contents"
    );
}

/// Asserts the push reported upstream's exit status for a daemon refusal.
///
/// This is the positive half of every scenario below. `RERR_PARTIAL` can only
/// appear once the module directive has matched a file and the receiver has
/// refused it; a daemon that ignored the directive would transfer all four
/// files and exit 0, and one that never served the module at all would fail
/// with a different status.
///
/// upstream: generator.c:1669-1671 reports the refusal as `FERROR_XFER`,
/// log.c:337-338 sets `got_xfer_error` on receipt, and cleanup.c:217-218 lifts
/// the exit status to `RERR_PARTIAL`.
fn assert_refused_exit_code(exit_code: Option<i32>) {
    assert_eq!(
        exit_code,
        Some(RERR_PARTIAL),
        "a push whose files the module refused must exit {RERR_PARTIAL} \
         (cleanup.c:217-218), not report success"
    );
}

/// Result of running one push-to-daemon scenario.
struct ScenarioOutcome {
    /// Destination-relative path -> contents, for every file that landed.
    files: BTreeMap<String, Vec<u8>>,
    /// The push's effective exit status; `None` when the transfer exited 0.
    exit_code: Option<i32>,
}

/// Configures a daemon with a single `[uploads]` module carrying the
/// supplied per-module directive block, populates a `src/` tree with two
/// `.txt` files (expected to land) and two `.log` files (expected to be
/// excluded), and pushes via the in-process client. Returns `None` if the
/// daemon could not bind (treated as a soft skip in CI sandboxes).
fn run_filter_scenario(
    test_name: &'static str,
    extra_directive_lines: &str,
) -> Option<ScenarioOutcome> {
    let _guard = TEST_LOCK.lock().expect("test lock poisoned");

    let Some((port, held_listener)) = allocate_test_port() else {
        eprintln!("{test_name}: skipped, no free port");
        return None;
    };

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::create_dir(&src).expect("create src");
    fs::create_dir(&dst).expect("create dst");

    // Two of each: a single accidentally-dropped file cannot pass.
    fs::write(src.join("keep1.txt"), b"keep me 1").expect("write keep1");
    fs::write(src.join("keep2.txt"), b"keep me 2").expect("write keep2");
    fs::write(src.join("drop1.log"), b"drop me 1").expect("write drop1");
    fs::write(src.join("drop2.log"), b"drop me 2").expect("write drop2");

    let config_path = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[uploads]\n\
         path = {dst}\n\
         use chroot = false\n\
         read only = false\n\
         {directives}\n",
        dst = dst.display(),
        directives = extra_directive_lines,
    );
    fs::write(&config_path, &config_content).expect("write rsyncd.conf");

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--max-sessions"),
            OsString::from("1"),
            // Mandatory. `RuntimeOptions::detach` defaults to `cfg!(unix)`, so
            // `run_daemon` reaches `become_daemon()` in the accept loop; the
            // parent of that fork is this very test process and it exits 0
            // (`platform::daemonize::become_daemon`). Without the flag the test
            // binary terminates successfully before a single assertion below
            // runs and the harness records a pass - an unconditional `panic!`
            // placed after this spawn still reported PASSED.
            OsString::from("--no-detach"),
        ])
        .pre_bound_listener(held_listener)
        .build();

    let daemon_handle = thread::spawn(move || run_daemon(daemon_config));

    // Build the client-side push directly through `core::client::run_client`
    // so the test exercises the same in-process orchestration as production
    // CLI invocations, without spawning a separate subprocess.
    let mut src_arg = src.clone().into_os_string();
    src_arg.push("/");
    let url = format!("rsync://127.0.0.1:{port}/uploads/");

    let client_config = ClientConfig::builder()
        .transfer_args([src_arg, OsString::from(&url)])
        .recursive(true)
        .build();

    let client_result = core::client::run_client(client_config);

    // The daemon was launched with `--max-sessions 1` so it returns to
    // `run_daemon`'s return point after the single transfer drains.
    let daemon_result = daemon_handle.join().expect("daemon thread panicked");

    if let Err(err) = daemon_result {
        panic!("{test_name}: daemon exited with error: {err:?}");
    }
    // A refused file is reported through the summary, not as an `Err`: the
    // status rides on `ClientSummary::io_error_exit_code`. An `Err` here means
    // the session itself broke down rather than that a file was filtered.
    let summary = match client_result {
        Ok(summary) => summary,
        Err(err) => panic!("{test_name}: client push failed: {err}"),
    };

    Some(ScenarioOutcome {
        files: collect_files(&dst),
        exit_code: summary.io_error_exit_code(),
    })
}

/// `filter = - *.log` strips `.log` files server-side, regardless of any
/// client-supplied filter. This proves the daemon-config injection site at
/// `module_access::transfer::serve_module` (`build_daemon_filter_rules` ->
/// `ServerConfig::daemon_filter_rules`) actually reaches the receiver's
/// `daemon_filter_set` and the per-file filter check in
/// `receiver/transfer/candidates.rs`.
#[test]
fn daemon_filter_directive_excludes_match_pattern() {
    let Some(outcome) = run_filter_scenario(
        "daemon_filter_directive_excludes_match_pattern",
        "filter = - *.log",
    ) else {
        return;
    };

    assert_kept(&outcome.files, "keep1.txt", b"keep me 1");
    assert_kept(&outcome.files, "keep2.txt", b"keep me 2");
    assert!(
        !outcome.files.contains_key("drop1.log"),
        "drop1.log must be excluded by `filter = - *.log`, got {:?}",
        outcome.files.keys().collect::<Vec<_>>()
    );
    assert!(
        !outcome.files.contains_key("drop2.log"),
        "drop2.log must be excluded by `filter = - *.log`, got {:?}",
        outcome.files.keys().collect::<Vec<_>>()
    );
    assert_refused_exit_code(outcome.exit_code);
}

/// `exclude = *.log` is the simple-exclude form upstream parses with
/// `FILTRULE_WORD_SPLIT` and no `FILTRULE_INCLUDE` flag. The destination must
/// be identical to the `filter = - *.log` form: both compile to the same
/// exclude rule.
#[test]
fn daemon_exclude_directive_excludes_match_pattern() {
    let Some(outcome) = run_filter_scenario(
        "daemon_exclude_directive_excludes_match_pattern",
        "exclude = *.log",
    ) else {
        return;
    };

    assert_kept(&outcome.files, "keep1.txt", b"keep me 1");
    assert_kept(&outcome.files, "keep2.txt", b"keep me 2");
    assert!(
        !outcome.files.contains_key("drop1.log"),
        "drop1.log must be excluded by `exclude = *.log`, got {:?}",
        outcome.files.keys().collect::<Vec<_>>()
    );
    assert!(
        !outcome.files.contains_key("drop2.log"),
        "drop2.log must be excluded by `exclude = *.log`, got {:?}",
        outcome.files.keys().collect::<Vec<_>>()
    );
    assert_refused_exit_code(outcome.exit_code);
}

/// `exclude from = <file>` loads patterns one-per-line. Upstream parses each
/// non-blank, non-`#`/`;` line via `parse_filter_file()` at
/// `clientserver.c:889-891`. The same end-state must hold: `.txt` lands,
/// `.log` is filtered before the receiver opens the temp file. The pattern
/// file deliberately mixes blank lines, `#` comments, and `;` comments to
/// guard against a future regression in `read_patterns_from_file()` skipping
/// the wrong sentinel.
#[test]
fn daemon_exclude_from_directive_loads_pattern_file() {
    let _guard = TEST_LOCK.lock().expect("test lock poisoned");

    let Some((port, held_listener)) = allocate_test_port() else {
        eprintln!("daemon_exclude_from_directive_loads_pattern_file: skipped, no free port");
        return;
    };

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    let patterns = temp.path().join("excludes.lst");
    fs::create_dir(&src).expect("create src");
    fs::create_dir(&dst).expect("create dst");

    fs::write(src.join("keep1.txt"), b"keep1").expect("write keep1");
    fs::write(src.join("keep2.txt"), b"keep2").expect("write keep2");
    fs::write(src.join("drop1.log"), b"drop1").expect("write drop1");
    fs::write(src.join("drop2.log"), b"drop2").expect("write drop2");

    // upstream: parse_filter_file() honours `#` / `;` comments and ignores
    // blank lines. Include all three flavours so any future regression in
    // `read_patterns_from_file()` would surface here, not in a downstream
    // interop run.
    fs::write(
        &patterns,
        b"# block-style comment\n\
          ; semicolon comment\n\
          \n\
          *.log\n",
    )
    .expect("write patterns");

    let config_path = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[uploads]\n\
         path = {dst}\n\
         use chroot = false\n\
         read only = false\n\
         exclude from = {patterns}\n",
        dst = dst.display(),
        patterns = patterns.display(),
    );
    fs::write(&config_path, &config_content).expect("write rsyncd.conf");

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--max-sessions"),
            OsString::from("1"),
            // Mandatory. `RuntimeOptions::detach` defaults to `cfg!(unix)`, so
            // `run_daemon` reaches `become_daemon()` in the accept loop; the
            // parent of that fork is this very test process and it exits 0
            // (`platform::daemonize::become_daemon`). Without the flag the test
            // binary terminates successfully before a single assertion below
            // runs and the harness records a pass - an unconditional `panic!`
            // placed after this spawn still reported PASSED.
            OsString::from("--no-detach"),
        ])
        .pre_bound_listener(held_listener)
        .build();

    let daemon_handle = thread::spawn(move || run_daemon(daemon_config));

    let mut src_arg = src.clone().into_os_string();
    src_arg.push("/");
    let url = format!("rsync://127.0.0.1:{port}/uploads/");

    let client_config = ClientConfig::builder()
        .transfer_args([src_arg, OsString::from(&url)])
        .recursive(true)
        .build();

    let client_result = core::client::run_client(client_config);
    let daemon_result = daemon_handle.join().expect("daemon thread panicked");

    if let Err(err) = daemon_result {
        panic!("daemon exited with error: {err:?}");
    }
    let summary = match client_result {
        Ok(summary) => summary,
        Err(err) => panic!("client push failed: {err}"),
    };

    let files = collect_files(&dst);
    assert_kept(&files, "keep1.txt", b"keep1");
    assert_kept(&files, "keep2.txt", b"keep2");
    assert!(
        !files.contains_key("drop1.log"),
        "drop1.log must be excluded by exclude-from file, got {:?}",
        files.keys().collect::<Vec<_>>()
    );
    assert!(
        !files.contains_key("drop2.log"),
        "drop2.log must be excluded by exclude-from file, got {:?}",
        files.keys().collect::<Vec<_>>()
    );
    assert_refused_exit_code(summary.io_error_exit_code());
}
