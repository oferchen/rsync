//! Regression: a protocol 28 `--dry-run` receiver requests regular files only.
//!
//! Below protocol 29 there are no iflags on the wire, so the peer's sender
//! reads every NDX as `ITEM_TRANSFER | ITEM_MISSING_DATA` and rejects one that
//! names a non-regular entry with `received request to transfer non-regular
//! file` (exit 2). Upstream's generator therefore writes nothing for a new
//! directory at protocol 28: `itemize()` logs it locally instead. Sending the
//! directory's NDX, as the protocol 29+ dry-run plan does, aborts every
//! `-n --protocol=28` push against upstream.
//!
//! # Upstream Reference
//!
//! - `generator.c:590,607-610` - `itemize()` writes NDX + iflags only at
//!   protocol >= 29; below that it calls `log_item()`.
//! - `generator.c:2376` - `notify_others` writes the NDX of a regular file
//!   that needs a transfer, even when `!do_xfers`.
//! - `rsync.c:384-385,436-443` - the sender implies `ITEM_TRANSFER` below
//!   protocol 29 and rejects a non-regular index.

use std::io::Cursor;
use std::num::NonZeroU8;

use protocol::codec::{NdxCodec, create_ndx_codec};
use protocol::flist::FileEntry;

use super::super::stats::TransferStats;
use super::super::transfer::mode::NonTransferMode;
use super::super::{PipelineSetup, ReceiverContext};
use super::support::test_handshake_with_protocol;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

/// Index of the regular file in the `[".", "newdir", "f"]` list.
const FILE_NDX: i32 = 2;

fn ndx_bytes(protocol: u8, ndx: i32) -> Vec<u8> {
    let mut buf = Vec::new();
    create_ndx_codec(protocol)
        .write_ndx(&mut buf, ndx)
        .expect("encode ndx");
    buf
}

/// Drives a server-side dry-run receive of `[".", "newdir", "f"]` into an
/// empty destination and returns the request bytes put on the wire. The
/// scripted peer echoes only the regular file's NDX, exactly what an upstream
/// protocol 28 sender answers (`write_ndx_and_attrs()` carries no iflags).
fn drive_dry_run(protocol: u8, echo: Vec<u8>) -> std::io::Result<Vec<u8>> {
    let dest = tempfile::TempDir::new().expect("tempdir");
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: protocol::ProtocolVersion::try_from(protocol).unwrap(),
        flags: ParsedServerFlags {
            dry_run: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&test_handshake_with_protocol(protocol), config);
    ctx.config.connection.client_mode = false;
    ctx.file_list = vec![
        FileEntry::new_directory(".".into(), 0o755),
        FileEntry::new_directory("newdir".into(), 0o755),
        FileEntry::new_file("f".into(), 4, 0o644),
    ];

    let setup = PipelineSetup {
        dest_dir: dest.path().to_path_buf(),
        metadata_opts: metadata::MetadataOptions::default(),
        checksum_length: NonZeroU8::new(2).expect("nonzero"),
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        acl_cache: None,
        acl_id_map: None,
        #[cfg(unix)]
        sandbox: None,
    };
    let files = vec![(FILE_NDX as usize, dest.path().join("f"), 0)];
    let mut reader = crate::reader::ServerReader::new_plain(Cursor::new(echo));
    let sent = SharedBuf::default();
    let mut writer = crate::writer::ServerWriter::new_plain(sent.clone());
    let mut stats = TransferStats::default();
    ctx.run_non_transfer_mode(
        NonTransferMode::DryRun,
        &mut reader,
        &mut writer,
        &setup,
        &files,
        &mut stats,
    )?;
    drop(writer);
    Ok(std::mem::take(&mut *sent.0.borrow_mut()))
}

/// Capture sink: `ServerWriter` owns its sink and exposes no accessor.
#[derive(Clone, Default)]
struct SharedBuf(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn protocol_28_dry_run_requests_only_the_regular_file() {
    let sent = drive_dry_run(28, ndx_bytes(28, FILE_NDX))
        .expect("a protocol 28 dry run must not request the new directory");
    assert_eq!(
        sent,
        ndx_bytes(28, FILE_NDX),
        "protocol 28 carries no iflags: only the regular file's NDX may cross"
    );
}

/// Control: at protocol 29 the new directory's NDX + iflags still crosses,
/// because the peer's sender renders that row from the iflags.
#[test]
fn protocol_29_dry_run_still_itemizes_the_new_directory() {
    let echo_with_iflags = |ndx: i32| {
        let mut buf = ndx_bytes(29, ndx);
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf
    };
    let mut echo = echo_with_iflags(1);
    echo.extend(echo_with_iflags(FILE_NDX));
    let sent = drive_dry_run(29, echo).expect("protocol 29 dry run completes");
    assert_eq!(
        &sent[..4],
        ndx_bytes(29, 1).as_slice(),
        "the new directory's NDX leads the protocol 29 requests"
    );
}
