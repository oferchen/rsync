//! Integration tests for `--debug=CHDIR` producer emissions exposed by the
//! `protocol::chdir` helper module.
//!
//! The trace helper wraps upstream rsync 3.4.1's sole `DEBUG_GTE(CHDIR, 1)`
//! emission from `util1.c:1168-1169` (`"[%s] change_dir(%s)\n"`). Upstream
//! routes every successful `chdir()` syscall through `change_dir`, so this
//! single helper covers the entire CHDIR producer surface.
//!
//! These tests drive the helper through the real [`logging`] channel - the
//! same path users hit when running `oc-rsync --debug=CHDIR`.

use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
use protocol::chdir::{ChdirRole, trace_change_dir};

/// Initialises logging with the supplied CHDIR debug level and drains any
/// pending events so the per-test assertions can focus on emissions produced
/// by the test body itself.
fn init_chdir(level: u8) {
    let mut cfg = VerbosityConfig::default();
    cfg.debug.chdir = level;
    init(cfg);
    let _ = drain_events();
}

/// Collects CHDIR-flagged debug messages emitted since the last drain.
fn chdir_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Chdir,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

/// Role tokens match upstream `who_am_i()` output byte-for-byte
/// (`rsync.c:823-830`).
#[test]
fn role_tokens_match_upstream_who_am_i() {
    assert_eq!(ChdirRole::Client.as_str(), "client");
    assert_eq!(ChdirRole::Server.as_str(), "server");
    assert_eq!(ChdirRole::Sender.as_str(), "sender");
    assert_eq!(ChdirRole::Generator.as_str(), "generator");
    assert_eq!(ChdirRole::Receiver.as_str(), "receiver");
    assert_eq!(ChdirRole::PreForkReceiver.as_str(), "Receiver");
}

/// `trace_change_dir` fires the upstream-format
/// `util1.c:1168-1169` emission at level 1 through the real logging channel.
#[test]
fn change_dir_emits_under_debug_chdir_level_1() {
    init_chdir(1);
    trace_change_dir(ChdirRole::Sender, "/data/src");
    let msgs = chdir_messages();
    assert!(
        msgs.iter().any(|m| m == "[sender] change_dir(/data/src)"),
        "expected CHDIR,1 sender emission, got {msgs:?}"
    );
}

/// The daemon's chroot-jail setup uses the upstream pre-fork-receiver role
/// token (`"Receiver"`, capitalised) and a post-chroot `curr_dir` of `"/"`.
/// This mirrors upstream `clientserver.c:987` `change_dir(module_chdir,
/// CD_NORMAL)` where, after `chroot()`, the cleaned absolute path becomes
/// the new root.
#[test]
fn daemon_chroot_emission_matches_upstream_pre_fork_token() {
    init_chdir(1);
    trace_change_dir(ChdirRole::PreForkReceiver, "/");
    let msgs = chdir_messages();
    assert!(
        msgs.iter().any(|m| m == "[Receiver] change_dir(/)"),
        "expected CHDIR,1 pre-fork receiver emission, got {msgs:?}"
    );
}

/// Paths with spaces and special characters render verbatim because the
/// upstream `rprintf(FINFO, "[%s] change_dir(%s)\n", ...)` does not quote
/// the path argument.
#[test]
fn paths_render_verbatim_without_quoting() {
    init_chdir(1);
    trace_change_dir(ChdirRole::Receiver, "/path with spaces");
    let msgs = chdir_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "[receiver] change_dir(/path with spaces)"),
        "expected verbatim path rendering, got {msgs:?}"
    );
}

/// Each `trace_change_dir` call emits exactly one CHDIR line.
#[test]
fn one_line_per_call() {
    init_chdir(1);
    trace_change_dir(ChdirRole::Generator, "/dst");
    trace_change_dir(ChdirRole::Receiver, "/dst");
    let msgs = chdir_messages();
    assert_eq!(
        msgs.len(),
        2,
        "expected one CHDIR line per call, got {msgs:?}"
    );
    assert_eq!(msgs[0], "[generator] change_dir(/dst)");
    assert_eq!(msgs[1], "[receiver] change_dir(/dst)");
}

/// Level 0 suppresses every CHDIR emission, mirroring upstream's
/// `DEBUG_GTE(CHDIR, 1)` gate.
#[test]
fn helper_silent_under_debug_chdir_level_0() {
    init_chdir(0);
    trace_change_dir(ChdirRole::Sender, "/data");
    trace_change_dir(ChdirRole::Receiver, "/dst");
    trace_change_dir(ChdirRole::PreForkReceiver, "/");
    assert!(
        chdir_messages().is_empty(),
        "all CHDIR emissions must be silent at level 0"
    );
}
