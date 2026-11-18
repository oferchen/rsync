//! A sender run with `--timeout` puts one empty `MSG_DATA` keepalive ahead of
//! everything it sends when its first act is to read, as rsync 3.5.2 does.
//!
//! upstream: `last_io_out` starts at 0 (io.c:113) and only perform_io()'s
//! write path sets it (io.c:975-976). After every read that needed input,
//! perform_io() calls `maybe_send_keepalive(last_io_in, 0)` (io.c:933-936),
//! which queues `send_msg(MSG_DATA, "", 0, 0)` once `now - last_io_out >=
//! allowed_lull` and nothing else is queued (io.c:1659-1666). A pull's
//! server is the sender, and it reads the client's filter list before it
//! writes anything, so the frame goes out first.
//!
//! The same rule says where the frame is absent, and each cell asserts that
//! layout for both implementations:
//!
//! - no `--timeout`: the call is skipped (io.c:933);
//! - a daemon at protocol 31 or later queues `MSG_IO_TIMEOUT` first
//!   (main.c:1315-1316), so a message is queued when the read comes;
//! - a push: the server is the receiver, whose reads before the fork are
//!   dropped by io.c:1656 and whose forked child writes to its generator, not
//!   the client. The generator's first poke is its per-entry check after the
//!   first entry (generator.c:2802, :2904-2913): with the clock still at 0 it
//!   flushes that entry's 3-byte itemize record on its own instead of sending
//!   a frame (io.c:1670-1672). That early flush is pinned per receiver driver
//!   below. A tree whose generator reads before its first entry (the `-H`
//!   lookahead, generator.c:2859-2862) makes 3.5.2 send the frame on a push
//!   too; that read has no counterpart here yet.
//!
//! The cells compare decoded frame layouts, not `--stats` byte totals: flush
//! timing moves where `MSG_DATA` frames break, and every extra break adds a
//! 4-byte header to the totals of an otherwise identical stream.

#![cfg(unix)]

use std::path::Path;

use test_support::transcript_cells::{CellBinaries, NetworkCell, build_fixture, capture_cell_with};
use test_support::{UpstreamVersion, require_upstream_rsync};

/// `MSG_DATA`'s code (upstream: rsync.h `MSG_DATA = 0`).
const MSG_DATA: u8 = 0;

/// One cell: a transfer shape, the protocol the client asks for (`None` for
/// the newest both sides speak) and whether `--timeout` is given.
struct Cell {
    shape: NetworkCell,
    protocol: Option<u32>,
    timeout: bool,
}

impl Cell {
    /// The server's frame indices that hold an empty `MSG_DATA`, as rsync
    /// 3.5.2 sends them (measured, and derived from the rule above).
    fn expected_keepalives(&self) -> Vec<usize> {
        let daemon = matches!(
            self.shape,
            NetworkCell::DaemonPush | NetworkCell::DaemonPull
        );
        let io_timeout_message = daemon && self.protocol.is_none_or(|p| p >= 31);
        let push = matches!(self.shape, NetworkCell::RshPush | NetworkCell::DaemonPush);
        if !self.timeout || io_timeout_message || push {
            Vec::new()
        } else {
            vec![0]
        }
    }

    fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(protocol) = self.protocol {
            args.push(format!("--protocol={protocol}"));
        }
        if self.timeout {
            args.push("--timeout=10".to_owned());
        }
        args
    }

    fn name(&self) -> String {
        format!(
            "{self_shape:?} protocol={proto} timeout={timeout}",
            self_shape = self.shape,
            proto = self
                .protocol
                .map_or_else(|| "newest".to_owned(), |p| p.to_string()),
            timeout = self.timeout
        )
    }
}

fn cells() -> Vec<Cell> {
    let mut cells = Vec::new();
    for shape in [
        NetworkCell::RshPull,
        NetworkCell::RshPush,
        NetworkCell::DaemonPull,
        NetworkCell::DaemonPush,
    ] {
        for protocol in [None, Some(30)] {
            for timeout in [false, true] {
                cells.push(Cell {
                    shape,
                    protocol,
                    timeout,
                });
            }
        }
    }
    cells
}

/// The indices of the empty `MSG_DATA` frames the server sent, and the
/// server's whole frame layout for the failure message.
fn keepalives(binary: &Path, cell: &Cell, src: &Path) -> (Vec<usize>, Vec<(u8, usize)>) {
    let binaries = CellBinaries {
        oc_rsync: binary,
        capture_rsh: Path::new(env!("CARGO_BIN_EXE_capture-rsh")),
    };
    let args = cell.args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let transcript = capture_cell_with(cell.shape, binaries, src, &args)
        .unwrap_or_else(|e| panic!("{}: {} capture failed: {e}", cell.name(), binary.display()));
    let frames = transcript
        .server_frames()
        .unwrap_or_else(|e| panic!("{}: server stream does not decode: {e}", cell.name()));
    let empty = frames
        .iter()
        .enumerate()
        .filter(|&(_, &(code, len))| code == MSG_DATA && len == 0)
        .map(|(index, _)| index)
        .collect();
    (empty, frames)
}

#[test]
fn a_server_with_timeout_sends_its_first_keepalive_like_rsync() {
    let fixture = test_support::create_tempdir();
    build_fixture(fixture.path(), false).expect("fixture");
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    let oc = test_support::oc_rsync_bin();
    let mut differ = Vec::new();
    for cell in cells() {
        let expected = cell.expected_keepalives();
        if let Some(upstream) = &upstream {
            let (reference, frames) = keepalives(upstream.binary(), &cell, fixture.path());
            assert_eq!(
                reference,
                expected,
                "{}: rsync 3.5.2 no longer matches the rule; its frames: {frames:?}",
                cell.name()
            );
        }
        let (ours, frames) = keepalives(&oc, &cell, fixture.path());
        if ours != expected {
            differ.push(format!(
                "{}: expected {expected:?}, got {ours:?} in {frames:?}",
                cell.name()
            ));
        }
    }
    assert!(
        differ.is_empty(),
        "{} cells differ:\n{}",
        differ.len(),
        differ.join("\n")
    );
}

/// The push server's receiver drivers: incremental recursion,
/// `--no-inc-recursive` and a dry run.
const PUSH_DRIVERS: [&[&str]; 3] = [&[], &["--no-inc-recursive"], &["-n"]];

/// The server's frames for one push.
fn push_frames(binary: &Path, cell: &Cell, driver: &[&str], src: &Path) -> Vec<(u8, usize)> {
    let binaries = CellBinaries {
        oc_rsync: binary,
        capture_rsh: Path::new(env!("CARGO_BIN_EXE_capture-rsh")),
    };
    let mut args = cell.args();
    args.extend(driver.iter().map(|arg| (*arg).to_owned()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let name = format!("{} {driver:?}", cell.name());
    capture_cell_with(cell.shape, binaries, src, &args)
        .unwrap_or_else(|e| panic!("{name}: {} capture failed: {e}", binary.display()))
        .server_frames()
        .unwrap_or_else(|e| panic!("{name}: server stream does not decode: {e}"))
}

/// Whether the first `MSG_DATA` frame is the first entry's 3-byte itemize
/// record flushed on its own, and the indices of the empty frames.
fn push_keepalive_layout(frames: &[(u8, usize)]) -> (bool, Vec<usize>) {
    let early_flush = frames
        .iter()
        .find(|&&(code, _)| code == MSG_DATA)
        .is_some_and(|&(_, len)| len == 3);
    let empty = frames
        .iter()
        .enumerate()
        .filter(|&(_, &(code, len))| code == MSG_DATA && len == 0)
        .map(|(index, _)| index)
        .collect();
    (early_flush, empty)
}

/// Each push cell compares two things only: the indices of empty `MSG_DATA`
/// frames, and whether the first `MSG_DATA` frame is the first entry's
/// 3-byte record flushed on its own. Whole frame lists are deliberately not
/// compared: the incremental and dry-run drivers break frames differently
/// from rsync even without `--timeout` (rsync's `maybe_flush_socket(0)`
/// cadence, generator.c:2912, io.c:1632-1637, is not ported, and the dry run
/// flushes after every record), so a whole-list comparison would fail for
/// reasons that are not the keepalive. For the same reason the early flush is
/// compared only on `--timeout` rows; without it rsync's early flush comes
/// from that unported path.
#[test]
fn a_push_server_with_timeout_flushes_its_first_entry_like_rsync() {
    let fixture = test_support::create_tempdir();
    build_fixture(fixture.path(), false).expect("fixture");
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    let oc = test_support::oc_rsync_bin();
    let mut differ = Vec::new();
    for shape in [NetworkCell::RshPush, NetworkCell::DaemonPush] {
        for protocol in [None, Some(30)] {
            for timeout in [false, true] {
                let cell = Cell {
                    shape,
                    protocol,
                    timeout,
                };
                let daemon_timeout_message =
                    shape == NetworkCell::DaemonPush && protocol.is_none_or(|p| p >= 31);
                for driver in PUSH_DRIVERS {
                    let name = format!("{} {driver:?}", cell.name());
                    let dry_run = driver.contains(&"-n");
                    // upstream: with --timeout the check after the first entry
                    // flushes it early unless the daemon's MSG_IO_TIMEOUT write
                    // already stamped the clock (io.c:1659, :1670-1672).
                    // Without --timeout the same spot calls maybe_flush_socket(0)
                    // instead (generator.c:2912), which flushes once the whole
                    // file list is in (io.c:1632-1637), so 3.5.2 flushes early
                    // under --no-inc-recursive too. That second path is not
                    // ported, so only the --timeout rows compare the early flush.
                    let reference_early = if timeout {
                        Some(!daemon_timeout_message)
                    } else if dry_run {
                        None
                    } else {
                        Some(driver.contains(&"--no-inc-recursive"))
                    };
                    if let Some(upstream) = &upstream {
                        let frames = push_frames(upstream.binary(), &cell, driver, fixture.path());
                        let (early, empty) = push_keepalive_layout(&frames);
                        assert!(
                            empty.is_empty(),
                            "{name}: rsync 3.5.2 sent an empty frame; its frames: {frames:?}"
                        );
                        if let Some(expected) = reference_early {
                            assert_eq!(
                                early, expected,
                                "{name}: rsync 3.5.2 no longer matches the rule; its frames: {frames:?}"
                            );
                        }
                    }
                    let frames = push_frames(&oc, &cell, driver, fixture.path());
                    let (early, empty) = push_keepalive_layout(&frames);
                    // A dry run here flushes after every record, so its first
                    // frame is 3 bytes whatever the keepalive does: it can only
                    // show the early flush where one is expected.
                    let compared = timeout && !(dry_run && reference_early == Some(false));
                    let early_differs = compared && Some(early) != reference_early;
                    if early_differs || !empty.is_empty() {
                        differ.push(format!(
                            "{name}: expected early flush {reference_early:?} and no empty frame, got {frames:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        differ.is_empty(),
        "{} cells differ:\n{}",
        differ.len(),
        differ.join("\n")
    );
}

/// Known difference, pinned on both sides: with `-H` and incremental
/// recursion, rsync's generator reads its receiver pipe before its first
/// entry, filling the hard-link lookahead (generator.c:2859-2862), and that
/// read queues an empty `MSG_DATA` while nothing has been written
/// (io.c:933-936), so rsync 3.5.2 leads with the frame. This implementation
/// has no lookahead read before its first entry, so it leads with the first
/// entry's early flush instead. Porting the lookahead makes this test fail,
/// and it must then become an equality row.
#[test]
fn a_hard_link_push_server_lacks_rsyncs_lookahead_keepalive() {
    let fixture = test_support::create_tempdir();
    build_fixture(fixture.path(), false).expect("fixture");
    let cell = Cell {
        shape: NetworkCell::RshPush,
        protocol: None,
        timeout: true,
    };
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let frames = push_frames(upstream.binary(), &cell, &["-H"], fixture.path());
        assert_eq!(
            push_keepalive_layout(&frames),
            (false, vec![0]),
            "rsync 3.5.2 no longer leads a -H push with the lookahead keepalive; its frames: {frames:?}"
        );
    }
    let frames = push_frames(
        &test_support::oc_rsync_bin(),
        &cell,
        &["-H"],
        fixture.path(),
    );
    assert_eq!(
        push_keepalive_layout(&frames),
        (true, Vec::new()),
        "the -H push now differs from the pinned layout; if it leads with an empty frame the lookahead read was ported, so compare with rsync instead: {frames:?}"
    );
}
