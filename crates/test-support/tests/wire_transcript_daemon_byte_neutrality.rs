//! Byte-neutrality transcript cells over the daemon protocol.
//!
//! The remote-shell gates (`wire_transcript_byte_neutrality.rs`,
//! `wire_transcript_pull_byte_neutrality.rs`) never reach the daemon's
//! transport: the `@RSYNCD:` exchange, the daemon's one-way negotiation and
//! its single full-duplex socket. These cells prove the daemon instrument
//! from `test_support::transcript_cells` before any I/O refactor relies on it:
//!
//! 1. Transport - the capture really is a daemon session: the server->client
//!    stream opens with the daemon greeting and the client->server stream
//!    names the module (upstream: clientserver.c start_inband_exchange()).
//! 2. Determinism - two runs of one binary on one fixture produce identical
//!    transcripts in both directions, for a push and for a pull.
//! 3. Sensitivity - a one-byte content change moves the stream that carries
//!    the file data (client->server on a push, server->client on a pull),
//!    and an independently rebuilt original fixture restores identity. The
//!    revert arm is what makes the gate able to fail.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::path::Path;

use test_support::WireTranscript;
use test_support::transcript_cells::{CellBinaries, NetworkCell, build_fixture, capture_cell};

const DAEMON_CELLS: [NetworkCell; 2] = [NetworkCell::DaemonPush, NetworkCell::DaemonPull];

fn capture(cell: NetworkCell, mutation: bool) -> WireTranscript {
    let src = test_support::create_tempdir();
    build_fixture(src.path(), mutation).expect("build fixture");
    let oc_rsync = test_support::oc_rsync_bin();
    let binaries = CellBinaries {
        oc_rsync: &oc_rsync,
        capture_rsh: Path::new(env!("CARGO_BIN_EXE_capture-rsh")),
    };
    let transcript =
        capture_cell(cell, binaries, src.path()).unwrap_or_else(|e| panic!("{cell}: {e}"));
    println!(
        "transcript[{cell}]: client->server {} bytes, server->client {} bytes",
        transcript.client_to_server.len(),
        transcript.server_to_client.len()
    );
    transcript
}

/// The stream that carries file content in `cell`.
fn data_stream(cell: NetworkCell, transcript: &WireTranscript) -> &[u8] {
    if cell == NetworkCell::DaemonPush {
        &transcript.client_to_server
    } else {
        &transcript.server_to_client
    }
}

#[test]
fn daemon_cells_capture_a_daemon_session() {
    for cell in DAEMON_CELLS {
        let transcript = capture(cell, false);
        assert!(
            transcript.server_to_client.starts_with(b"@RSYNCD: "),
            "{cell}: the server->client stream must open with the daemon greeting"
        );
        assert!(
            transcript
                .client_to_server
                .windows(b"cell\n".len())
                .any(|w| w == b"cell\n"),
            "{cell}: the client must request the module by name"
        );
    }
}

#[test]
fn daemon_transcripts_are_deterministic() {
    for cell in DAEMON_CELLS {
        let first = capture(cell, false);
        let second = capture(cell, false);
        assert!(
            first.client_to_server == second.client_to_server,
            "{cell}: client->server differs between identical runs ({} vs {} bytes)",
            first.client_to_server.len(),
            second.client_to_server.len()
        );
        assert!(
            first.server_to_client == second.server_to_client,
            "{cell}: server->client differs between identical runs ({} vs {} bytes)",
            first.server_to_client.len(),
            second.server_to_client.len()
        );
    }
}

#[test]
fn daemon_transcripts_see_a_one_byte_change_and_revert() {
    for cell in DAEMON_CELLS {
        let base = capture(cell, false);
        let mutated = capture(cell, true);
        assert!(
            data_stream(cell, &base) != data_stream(cell, &mutated),
            "{cell}: a one-byte content change must move the data stream"
        );
        let reverted = capture(cell, false);
        assert!(
            base == reverted,
            "{cell}: a rebuilt original fixture must restore byte identity"
        );
    }
}
