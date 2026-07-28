//! Wire shape of the server-mode metadata-only itemize records on a push.
//!
//! On a push the remote receiver's generator owns the itemize decision, but
//! the CLIENT's sender owns the printing: upstream writes `NDX +
//! write_shortint(iflags)` for every quick-check-matched entry whose
//! attributes still differ (`generator.c:582-593`), the sender prints the row
//! (`sender.c:292-293 maybe_log_item`) and echoes the attrs back
//! (`sender.c:294 write_ndx_and_attrs`). These tests pin the record's exact
//! wire bytes and prove the pipeline drains the sender's echo, so a
//! metadata-only run cannot desync the phase-done handshake.

use std::io::{Cursor, Read};
use std::num::NonZeroU8;
use std::path::PathBuf;

use protocol::codec::{NdxCodec, create_ndx_codec};
use protocol::flist::FileEntry;

use super::super::{PipelineSetup, ReceiverContext};
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::{InfoFlags, ParsedServerFlags};
use crate::generator::ItemFlags;
use crate::role::ServerRole;

/// Capture sink for the bytes the receiver puts on the wire. `ServerWriter`
/// owns its sink and exposes no accessor, so the buffer is shared.
#[derive(Clone, Default)]
struct SharedBuf(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl SharedBuf {
    fn take(self) -> Vec<u8> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A server-mode (push) receiver with `-i` requested by the client.
fn push_receiver() -> ReceiverContext {
    let handshake = test_handshake();
    let mut config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        flags: ParsedServerFlags {
            info_flags: InfoFlags {
                itemize: true,
                ..InfoFlags::default()
            },
            ..ParsedServerFlags::default()
        },
        ..Default::default()
    };
    config.connection.client_mode = false;
    ReceiverContext::new_for_test(&handshake, config)
}

/// The sender's echo of one non-transfer record: `NDX + iflags`, nothing else.
fn sender_echo(ndx: i32, iflags: u16) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut codec = create_ndx_codec(32);
    codec.write_ndx(&mut buf, ndx).expect("write echo ndx");
    buf.extend_from_slice(&iflags.to_le_bytes());
    buf
}

/// A recorded metadata-only row must reach the wire as exactly
/// `NDX + write_shortint(iflags)` - no sum head, basis byte, or xname - and
/// its echo must be consumed by the same loop, in FIFO order. Pre-fix, the
/// pipeline dropped the record entirely: the pushing client's sender had
/// nothing to render and a chmod-only push printed no row at all.
#[test]
fn server_push_pipeline_forwards_metadata_only_record_and_drains_echo() {
    let mut ctx = push_receiver();
    ctx.file_list = vec![FileEntry::new_file("f".into(), 1, 0o600)];
    let iflags = ItemFlags::ITEM_REPORT_PERMS as u16;
    ctx.server_no_transfer_itemize
        .borrow_mut()
        .push((0, iflags));

    let dir = test_support::create_tempdir();
    let setup = PipelineSetup {
        dest_dir: dir.path().to_path_buf(),
        metadata_opts: metadata::MetadataOptions::default(),
        checksum_length: NonZeroU8::new(2).expect("nonzero"),
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        acl_cache: None,
        acl_id_map: None,
        #[cfg(unix)]
        sandbox: None,
    };

    let mut reader = crate::reader::ServerReader::new_plain(Cursor::new(sender_echo(0, iflags)));
    let sent = SharedBuf::default();
    let mut writer = crate::writer::ServerWriter::new_plain(sent.clone());
    let mut metadata_errors = Vec::new();
    let files: Vec<(usize, &FileEntry, PathBuf, u32)> = Vec::new();
    let result = ctx
        .run_pipeline_loop_decoupled(
            &mut reader,
            &mut writer,
            crate::pipeline::PipelineConfig::default(),
            &setup,
            files,
            &mut metadata_errors,
            false,
            0,
            &mut None,
        )
        .expect("metadata-only phase must complete without hanging on the echo");

    assert_eq!(result.0, 0, "a metadata-only record transfers no file");
    assert!(
        ctx.server_no_transfer_itemize.borrow().is_empty(),
        "the phase-1 loop must consume the recorded rows"
    );

    // Decode with the peer sender's own reader path so this fails if the
    // emitted bytes ever diverge from what the sender consumes.
    let bytes = sent.take();
    let mut cur = Cursor::new(bytes.as_slice());
    let mut rd = create_ndx_codec(32);
    assert_eq!(
        rd.read_ndx(&mut cur).expect("record NDX must decode"),
        0,
        "the record carries the entry's flist NDX"
    );
    let mut wire_iflags = [0u8; 2];
    cur.read_exact(&mut wire_iflags).expect("iflags shortint");
    assert_eq!(
        u16::from_le_bytes(wire_iflags),
        iflags,
        "the shortint carries the attribute-diff bits the client renders"
    );
    assert_eq!(
        cur.position() as usize,
        bytes.len(),
        "no sum head or trailing fields may follow a metadata-only record"
    );
}
