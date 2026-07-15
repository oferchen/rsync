//! File-list surface: receiving raw and incremental file lists, sender
//! attribute decoding, sum-head wire format, NDX-segment conversion, id
//! lists, directory creation, the receiver-side filter chain that gates
//! deletions, and the delete-pipeline hook fed by INC_RECURSE segments.
//!
//! Split into per-concern submodules to keep each file focused and within
//! the 650-line cap:
//!
//! - [`wire_attrs`] - `SumHead` and `SenderAttrs` wire-format coverage.
//! - [`id_lists`] - `receive_id_lists` gating by owner/group/numeric-ids.
//! - [`incremental_receiver`] - `IncrementalFileListReceiver::try_read_one`
//!   and its pending/ready buffering semantics.
//! - [`incremental_directories`] - higher-level incremental flow:
//!   `next_ready`, `collect_sorted`, `create_directory_incremental`, and
//!   pipelined-incremental compile checks.
//! - [`filter_chain`] - receiver-side filter chain that protects entries
//!   from deletion.
//! - [`proto_io_error`] - protocol 28/29 trailing `io_error` int after
//!   the file-list end marker.
//! - [`ndx_convert`] - `flat_to_wire_ndx` / `wire_to_flat_ndx` counters
//!   and round-trip coverage.
//! - [`delete_pipeline_hook`] - DDP-B3 hook that publishes one delete
//!   plan per INC_RECURSE segment.
//! - [`delete_timing`] - early vs late (`--delete-after` / `--delete-delay`)
//!   delete-pass scheduling and the dest-side `.rsync-filter` protection
//!   invariant that makes deferral load-bearing.
//! - [`iconv_wire_order`] - regression coverage for the receiver-side
//!   `--iconv` ordering invariant (file_list stays in sender wire-emit
//!   order, never re-sorted on local-charset bytes).

#[cfg(feature = "tokio-transfer")]
mod async_parity;
mod delete_pipeline_hook;
mod delete_timing;
mod filter_chain;
mod filter_recheck;
#[cfg(feature = "iconv")]
mod iconv_wire_order;
mod id_lists;
mod incremental_directories;
mod incremental_receiver;
mod missing_args_sentinel;
mod ndx_convert;
mod proto_io_error;
mod wire_attrs;

use std::io::Cursor;

use super::super::ReceiverContext;
use super::support::{test_config, test_handshake};

#[test]
fn receiver_context_creation() {
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    assert_eq!(ctx.protocol().as_u8(), 32);
    assert!(ctx.file_list().is_empty());
}

#[test]
fn receiver_empty_file_list() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Empty file list (just the end marker)
    let data = [0u8];
    let mut cursor = Cursor::new(&data[..]);

    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert!(ctx.file_list().is_empty());
}

#[test]
fn receiver_single_file() {
    use protocol::flist::{FileEntry, FileListWriter};

    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Create a proper file list using FileListWriter for protocol 32
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(handshake.protocol);

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &entry).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let count = ctx.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 1);
    assert_eq!(ctx.file_list().len(), 1);
    assert_eq!(ctx.file_list()[0].name(), "test.txt");
}
