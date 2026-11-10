//! A signal delivered to an rsh `--server` mid-transfer.
//!
//! upstream: main.c:1919-1921 installs `sig_int` for SIGINT, SIGHUP and
//! SIGTERM in every role. A server records the signal (rsync.c:864-868) and
//! its I/O loop acts on it at the next pass (io.c:849/997/1022 ->
//! io.c:600-605 handle_kill_signal() -> exit_cleanup(RERR_SIGNAL)), so the
//! transfer stops at the next frame, the in-flight temp is kept under
//! `--partial` (mtime 0 without `--partial-dir`, cleanup.c:159-181) or
//! unlinked otherwise (cleanup.c:200-201), and the client leaves with
//! `RERR_SIGNAL` (20) after the server's
//! `received SIGINT, SIGTERM, or SIGHUP (code 20)` line.
//!
//! The `signal-rsh` trampoline signals the server once a fixed number of
//! bytes has crossed the wire, then keeps relaying: the point of delivery is
//! a byte offset, not a sleep. Oracle (rsync 3.5.1 server, same cells, with
//! the 3.5.1 and the oc client): exit 20, no `.z.*` temp left, a shortened
//! `z` only with `--partial`.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use test_support::upstream_compat::{
    UpstreamVersion, locate_upstream_rsync, require_upstream_rsync,
};

/// Source size; the signal lands after [`SIGNAL_AT`] bytes, far before the end.
///
/// Large enough that a server which only notices the signal between files
/// (or once it next reads) relays nearly all of it: the kept prefix must stay
/// under half of it.
const FILE_SIZE: usize = 32 << 20;
const SIGNAL_AT: u64 = 1 << 20;
const RERR_SIGNAL: i32 = 20;

#[derive(Clone, Copy)]
enum Keep {
    /// No `--partial`: the temp is unlinked.
    Nothing,
    /// `--partial`: the prefix lands on the destination with mtime 0.
    Partial,
    /// `--partial-dir=.pd`: the prefix lands in the partial dir.
    PartialDir,
}

struct Cell {
    push: bool,
    keep: Keep,
    signal: &'static str,
    /// Bytes of the signalled direction relayed before the signal.
    at: u64,
}

impl Cell {
    const fn new(push: bool, keep: Keep) -> Self {
        Self {
            push,
            keep,
            signal: "INT",
            at: SIGNAL_AT,
        }
    }

    /// who_am_i() of the signalled server process once the transfer runs.
    const fn role(&self) -> &'static str {
        if self.push { "generator" } else { "sender" }
    }
}

fn source_tree(root: &Path) -> Vec<u8> {
    // A xorshift stream: incompressible and not matchable, so every byte is literal.
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let data: Vec<u8> = (0..FILE_SIZE)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("dst")).unwrap();
    fs::write(root.join("src/z"), &data).unwrap();
    data
}

/// Runs `client` against an oc server that receives the cell's signal.
fn transfer(client: &Path, root: &Path, cell: &Cell) -> Output {
    let (src, dst) = if cell.push {
        (
            format!("{}/", root.join("src").display()),
            format!("host:{}/", root.join("dst").display()),
        )
    } else {
        (
            format!("host:{}/", root.join("src").display()),
            format!("{}/", root.join("dst").display()),
        )
    };
    // A push server reads the client's data; a pull server sends it.
    let direction = if cell.push { "c2s" } else { "s2c" };
    let mut cmd = Command::new(client);
    cmd.args(["-r", "-t", "-e"])
        .arg(env!("CARGO_BIN_EXE_signal-rsh"))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .env("OC_SIGNAL_RSH_SIGNAL", cell.signal)
        .env("OC_SIGNAL_RSH_AT", format!("{direction}:{}", cell.at));
    match cell.keep {
        Keep::Nothing => {}
        Keep::Partial => {
            cmd.arg("--partial");
        }
        Keep::PartialDir => {
            cmd.arg("--partial-dir=.pd");
        }
    }
    cmd.arg(src).arg(dst).output().expect("run the client")
}

fn temps(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with(".z."))
                .collect()
        })
        .unwrap_or_default()
}

/// Asserts the 3.5.1 outcome for one cell.
fn assert_signalled(client: &Path, cell: &Cell) {
    let dir = test_support::create_tempdir();
    let source = source_tree(dir.path());
    let out = transfer(client, dir.path(), cell);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let dst = dir.path().join("dst");

    assert_eq!(out.status.code(), Some(RERR_SIGNAL), "stderr: {stderr}");
    assert!(
        stderr.contains("received SIGINT, SIGTERM, or SIGHUP (code 20) at")
            && stderr.contains(&format!("[{}=", cell.role())),
        "the server must report the signal as its {}: {stderr}",
        cell.role()
    );
    assert_eq!(temps(&dst), Vec::<String>::new(), "no temp may survive");
    assert_eq!(temps(&dst.join(".pd")), Vec::<String>::new());

    let kept_at = match cell.keep {
        Keep::Nothing => {
            assert!(!dst.join("z").exists(), "without --partial nothing is kept");
            return;
        }
        Keep::Partial => dst.join("z"),
        Keep::PartialDir => {
            assert!(
                !dst.join("z").exists(),
                "the partial goes to the partial dir"
            );
            dst.join(".pd/z")
        }
    };
    let kept = fs::read(&kept_at).expect("the received prefix is kept");
    assert!(
        !kept.is_empty() && kept.len() <= source.len() / 2,
        "the server must stop at the next frame, not run on: kept {} of {}",
        kept.len(),
        source.len()
    );
    assert_eq!(kept, source[..kept.len()], "the partial is a prefix");
    let mtime = fs::metadata(&kept_at)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // upstream: cleanup.c:171-178 - only a partial on the destination itself
    // is stamped with mtime 0, so --update cannot skip it.
    assert_eq!(
        mtime == 0,
        matches!(cell.keep, Keep::Partial),
        "partial mtime {mtime}"
    );
}

/// The clients the cells run under: oc, and 3.5.1 when it is installed.
///
/// A 3.5.1 binary that resolves but fails the banner check would silently
/// drop its cells, so it fails the test instead of skipping it.
fn clients() -> Vec<PathBuf> {
    let mut clients = vec![test_support::oc_rsync_bin()];
    let located = locate_upstream_rsync(UpstreamVersion::V3_5_1);
    match require_upstream_rsync(UpstreamVersion::V3_5_1) {
        Some(upstream) => clients.push(upstream.binary().to_path_buf()),
        None => assert!(
            located.is_none(),
            "3.5.1 at {located:?} was found but rejected; its cells would not run"
        ),
    }
    clients
}

/// Runs the cell under every client and returns how many ran, printing each
/// so a run's log shows which clients the cells covered.
fn each_client(cell: &Cell) -> usize {
    let clients = clients();
    for client in &clients {
        eprintln!("server_signal: client {}", client.display());
        assert_signalled(client, cell);
    }
    clients.len()
}

#[test]
fn sigint_to_a_push_server_unlinks_its_temp() {
    let _ = each_client(&Cell::new(true, Keep::Nothing));
}

#[test]
fn sigint_to_a_push_server_keeps_the_partial_with_mtime_zero() {
    let _ = each_client(&Cell::new(true, Keep::Partial));
}

#[test]
fn sigint_to_a_push_server_keeps_the_partial_dir_entry() {
    let _ = each_client(&Cell::new(true, Keep::PartialDir));
}

#[test]
fn sigterm_and_sighup_reach_the_same_cleanup() {
    for signal in ["TERM", "HUP"] {
        let _ = each_client(&Cell {
            signal,
            ..Cell::new(true, Keep::Nothing)
        });
    }
}

#[test]
fn sigint_to_a_pull_server_stops_the_sender() {
    let _ = each_client(&Cell::new(false, Keep::Nothing));
}

#[test]
fn sigint_to_a_pull_server_leaves_the_client_partial() {
    let _ = each_client(&Cell::new(false, Keep::Partial));
}

/// upstream: io.c:2692 arms got_kill_signal only once multiplexed I/O
/// starts; a signal during the version exchange takes rsync.c:871's
/// immediate exit_cleanup(RERR_SIGNAL). 8 client bytes are the version
/// plus the first post-version word, so the server is signalled before any
/// file data. Oracle: 3.5.1 and oc servers both exit 20, nothing created.
#[test]
fn sigint_during_the_handshake_exits_20_without_a_file() {
    for client in clients() {
        let dir = test_support::create_tempdir();
        source_tree(dir.path());
        let cell = Cell {
            at: 8,
            ..Cell::new(true, Keep::Nothing)
        };
        let out = transfer(&client, dir.path(), &cell);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(RERR_SIGNAL), "stderr: {stderr}");
        assert!(
            stderr.contains("received SIGINT, SIGTERM, or SIGHUP (code 20)"),
            "{stderr}"
        );
        let dst = dir.path().join("dst");
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 0, "nothing created");
    }
}

/// Pins that the cells above ran under 3.5.1 too when it is installed, so a
/// lookup regression cannot turn the upstream half into a silent pass.
#[test]
fn the_cells_run_under_both_clients_when_3_5_1_is_installed() {
    let expected = 1 + usize::from(locate_upstream_rsync(UpstreamVersion::V3_5_1).is_some());
    assert_eq!(
        each_client(&Cell::new(true, Keep::Nothing)),
        expected,
        "clients covered"
    );
}
