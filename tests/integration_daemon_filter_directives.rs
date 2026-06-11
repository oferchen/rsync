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
//! upstream: clientserver.c:874-893 - `rsync_module()` builds
//! `daemon_filter_list` from `filter` / `include_from` / `include` /
//! `exclude_from` / `exclude` in that order, then `check_filter()` at
//! `receiver.c:711-714` and `generator.c:1273-1275` consults it before any
//! per-file action.
//!
//! Gated `#[cfg(unix)]` because the in-process client + daemon split uses
//! POSIX TCP and the helper walks the destination with `read_dir`; the test
//! also skips silently when ephemeral port allocation fails (sandboxed CI),
//! mirroring the pattern from `integration_daemon_max_connections_cap.rs`
//! and the daemon crate's `daemon_itemize_push` chunk.

#![cfg(unix)]

use std::collections::HashSet;
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

/// Allocates a free TCP port for the test daemon. Returns both the port and
/// the bound `TcpListener` so the listener can be handed to the daemon via
/// `pre_bound_listener` - this closes the TOCTOU window between port
/// allocation and the daemon's own bind.
fn allocate_test_port() -> Option<(u16, TcpListener)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    Some((port, listener))
}

/// Walks `root` recursively and returns the set of relative file paths. Used
/// to assert the destination contains exactly the files we expect.
fn collect_files(root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
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
                if let Ok(rel) = path.strip_prefix(root) {
                    out.insert(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    out
}

/// Result of running one push-to-daemon scenario.
struct ScenarioOutcome {
    files: HashSet<String>,
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

    if let Err(err) = client_result {
        panic!("{test_name}: client push failed: {err}");
    }
    if let Err(err) = daemon_result {
        panic!("{test_name}: daemon exited with error: {err:?}");
    }

    Some(ScenarioOutcome {
        files: collect_files(&dst),
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

    assert!(
        outcome.files.contains("keep1.txt"),
        "keep1.txt must be transferred, got {:?}",
        outcome.files
    );
    assert!(
        outcome.files.contains("keep2.txt"),
        "keep2.txt must be transferred, got {:?}",
        outcome.files
    );
    assert!(
        !outcome.files.contains("drop1.log"),
        "drop1.log must be excluded by `filter = - *.log`, got {:?}",
        outcome.files
    );
    assert!(
        !outcome.files.contains("drop2.log"),
        "drop2.log must be excluded by `filter = - *.log`, got {:?}",
        outcome.files
    );
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

    assert!(
        outcome.files.contains("keep1.txt"),
        "keep1.txt missing: {:?}",
        outcome.files
    );
    assert!(
        outcome.files.contains("keep2.txt"),
        "keep2.txt missing: {:?}",
        outcome.files
    );
    assert!(
        !outcome.files.contains("drop1.log"),
        "drop1.log must be excluded by `exclude = *.log`, got {:?}",
        outcome.files
    );
    assert!(
        !outcome.files.contains("drop2.log"),
        "drop2.log must be excluded by `exclude = *.log`, got {:?}",
        outcome.files
    );
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

    if let Err(err) = client_result {
        panic!("client push failed: {err}");
    }
    if let Err(err) = daemon_result {
        panic!("daemon exited with error: {err:?}");
    }

    let files = collect_files(&dst);
    assert!(files.contains("keep1.txt"), "keep1.txt missing: {files:?}");
    assert!(files.contains("keep2.txt"), "keep2.txt missing: {files:?}");
    assert!(
        !files.contains("drop1.log"),
        "drop1.log must be excluded by exclude-from file, got {files:?}"
    );
    assert!(
        !files.contains("drop2.log"),
        "drop2.log must be excluded by exclude-from file, got {files:?}"
    );
}
