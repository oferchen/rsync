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
//!   the client. 3.5.2's frame can then only come from the generator's first
//!   read of the receiver's pipe; with this fixture (`-rt`, a nested tree) the
//!   generator has already buffered its first file's request by then, so
//!   nothing is queued. A tree whose generator reads first makes 3.5.2 send
//!   the frame on a push too; this implementation, whose roles are threads
//!   with no such pipe, does not.
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
