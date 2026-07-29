//! `File list size` raw-span accounting on the receiving side.
//!
//! Upstream measures `stats.flist_size` as the delta of the raw wire read
//! counter across every `recv_file_list()` span (`flist.c:2615` snapshots
//! `stats.total_read`, `flist.c:2789` accumulates the delta). The figure is
//! never sent over the wire - each role prints its own local number
//! (`main.c:445`) - so a pulling client that never measures the span prints
//! `File list size: 0` on every remote transfer. These tests pin the span
//! measurement against the same raw-counter layering production uses.

use std::io::{Cursor, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListWriter};

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake, test_handshake_with_protocol};
use crate::config::ServerConfig;
use crate::flags::{NumericIds, ParsedServerFlags};
use crate::role::ServerRole;

/// Raw-level counting shim mirroring the production `CountingReader`
/// (upstream: io.c:820 `stats.total_read += n`).
struct CountingRead<'a> {
    inner: Cursor<&'a [u8]>,
    counter: Arc<AtomicU64>,
}

impl Read for CountingRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.counter.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Encodes entries plus the end marker into a wire buffer.
fn encode(protocol: ProtocolVersion, entries: &[FileEntry]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in entries {
        writer.write_entry(&mut data, entry).unwrap();
    }
    writer.write_end(&mut data, None).unwrap();
    data
}

/// The measured span must equal the raw bytes the list occupied on the wire.
///
/// WHY: upstream's `File list size` is the raw-counter delta across
/// `recv_file_list()` (flist.c:2615/2789), so the pulling client's printed
/// value equals the wire bytes consumed for the list. Before the fix the
/// receiver never measured the span and every remote pull printed
/// `File list size: 0` while upstream printed the real size.
#[test]
fn flist_size_counts_raw_wire_span() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let entries = [
        FileEntry::new_file("a.txt".into(), 100, 0o644),
        FileEntry::new_file("b.txt".into(), 50, 0o644),
        FileEntry::new_file("sub/c.txt".into(), 10, 0o644),
    ];
    let data = encode(handshake.protocol, &entries);

    let counter = Arc::new(AtomicU64::new(0));
    ctx.set_raw_read_counter(Arc::clone(&counter));

    let mut reader = CountingRead {
        inner: Cursor::new(&data[..]),
        counter: Arc::clone(&counter),
    };
    let count = ctx.receive_file_list(&mut reader).unwrap();

    assert_eq!(count, 3);
    assert_eq!(
        ctx.flist_size(),
        data.len() as u64,
        "flist_size must equal the raw wire bytes of the list span"
    );
}

/// Without an attached raw counter the span is unmeasured and stays 0.
///
/// WHY: server-side receivers never print `File list size` (upstream sends
/// only total_read/total_written/total_size in the stats trailer,
/// main.c:348-355), so the measurement is opt-in via the counter handle and
/// its absence must not disturb the read path.
#[test]
fn flist_size_zero_without_counter() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let entries = [FileEntry::new_file("a.txt".into(), 100, 0o644)];
    let data = encode(handshake.protocol, &entries);

    let mut cursor = Cursor::new(&data[..]);
    let count = ctx.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 1);
    assert_eq!(ctx.flist_size(), 0);
}

/// The protocol < 30 trailing io_error int is part of the measured span.
///
/// WHY: upstream reads the 4-byte io_error trailer inside `recv_file_list()`
/// (flist.c:2773-2777) before accumulating the span at flist.c:2789, so those
/// bytes count toward `File list size`.
#[test]
fn flist_size_includes_pre30_io_error_int() {
    let handshake = test_handshake_with_protocol(28);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(28u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: NumericIds::Explicit,
            ..Default::default()
        },
        args: vec![std::ffi::OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Wire: 0x00 end marker + 4-byte LE io_error trailer.
    let mut wire = vec![0x00u8];
    wire.extend_from_slice(&0i32.to_le_bytes());

    let counter = Arc::new(AtomicU64::new(0));
    ctx.set_raw_read_counter(Arc::clone(&counter));

    let mut reader = CountingRead {
        inner: Cursor::new(&wire[..]),
        counter: Arc::clone(&counter),
    };
    let count = ctx.receive_file_list(&mut reader).unwrap();

    assert_eq!(count, 0);
    assert_eq!(
        ctx.flist_size(),
        wire.len() as u64,
        "the pre-30 io_error trailer is inside the measured span"
    );
}
