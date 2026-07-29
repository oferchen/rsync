//! `Total bytes sent` / `Total bytes received` accounting on a client pull.
//!
//! Upstream's `handle_stats()` makes the sender authoritative: it caches the
//! raw descriptor counters `stats.total_read` (io.c:820) and
//! `stats.total_written` (io.c:859) and, when it is a server sender, writes them
//! over the wire (main.c:349-350). The client receiver reads them back and
//! swaps their meaning (main.c:365-372): it prints the sender's `total_read` as
//! `Total bytes sent` and the sender's `total_written` as `Total bytes
//! received`. These tests pin that the receiver captures the transmitted trailer
//! and exposes it for the swap, rather than reporting its own local wire tally
//! (which diverges by the trailing stats and goodbye bytes).

use std::io::Cursor;

use protocol::TransferStats;

use super::super::ReceiverContext;
use super::support::{test_config, test_handshake};

/// The receiver decodes the sender's transmitted counters in `total_read`,
/// `total_written` order, and the client swap maps them onto the summary.
///
/// WHY: a client pull must print the sender's numbers exactly. Sourcing them
/// from the wire trailer (not the receiver's own counters) is what keeps
/// `Total bytes sent`/`received` byte-identical with the sender, because the
/// receiver's local tally additionally counts the trailer and goodbye bytes the
/// sender wrote after it sampled at its `handle_stats` point.
#[test]
fn receive_stats_decodes_sender_counters_for_the_client_swap() {
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    // The server sender's `send_stats` encodes (total_read, total_written,
    // total_size) via this exact call (generator/transfer/stats.rs).
    let sender_total_read = 96u64;
    let sender_total_written = 9_780u64;
    let sender_total_size = 8_500u64;
    let mut wire = Vec::new();
    TransferStats::with_bytes(sender_total_read, sender_total_written, sender_total_size)
        .with_flist_times(0, 0)
        .write_to(&mut wire, handshake.protocol)
        .unwrap();

    let mut reader = Cursor::new(&wire[..]);
    let sender_stats = ctx.receive_stats(&mut reader).unwrap();

    assert_eq!(sender_stats.total_read, sender_total_read);
    assert_eq!(sender_stats.total_written, sender_total_written);
    assert_eq!(sender_stats.total_size, sender_total_size);

    // upstream: main.c:365-372,454-457 - the client receiver's report swaps the
    // pair: "Total bytes sent" is the sender's total_read (what the client sent,
    // as the sender read it), "Total bytes received" is the sender's
    // total_written (what the client received, as the sender wrote it).
    let client_bytes_sent = sender_stats.total_read;
    let client_bytes_received = sender_stats.total_written;
    assert_eq!(client_bytes_sent, 96);
    assert_eq!(client_bytes_received, 9_780);
}

/// `sender_stats()` is `None` until a trailer is captured, and stores it after.
///
/// WHY: a server receiver (a push's remote side) never reads a stats trailer,
/// so the field must stay `None` there and the client swap must fall back to the
/// local tally - matching upstream's `if (f < 0 && !am_sender)` no-op
/// (main.c:363). Only a client pull that captured a trailer overrides the report.
#[test]
fn sender_stats_starts_none_and_retains_the_captured_trailer() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    assert!(ctx.sender_stats().is_none());

    let mut wire = Vec::new();
    TransferStats::with_bytes(42, 4_242, 100)
        .with_flist_times(0, 0)
        .write_to(&mut wire, handshake.protocol)
        .unwrap();
    let captured = ctx.receive_stats(&mut Cursor::new(&wire[..])).unwrap();
    // `finalize_transfer` performs exactly this assignment on a client pull.
    ctx.sender_stats = Some(captured);

    let stored = ctx.sender_stats().expect("trailer retained");
    assert_eq!(stored.total_read, 42);
    assert_eq!(stored.total_written, 4_242);
}
