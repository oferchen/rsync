//! `got_xfer_error`: a `MSG_ERROR_XFER` frame is the only report a missing
//! source argument produces.
//!
//! Upstream's sender deliberately withholds `IOERR_GENERAL` when `link_stat()`
//! fails with `ENOENT` (`flist.c:2428-2436`):
//!
//! ```c
//! if (errno != ENOENT || missing_args == 0) {
//!         /* This is a transfer error, but inhibit deletion
//!          * only if we might be omitting an existing file. */
//!         if (errno != ENOENT)
//!                 io_error |= IOERR_GENERAL;
//!         rsyserr(FERROR_XFER, errno, "link_stat %s failed", full_fname(fbuf));
//!         continue;
//! }
//! ```
//!
//! The `io_error` word the receiver reads off the wire therefore stays clear,
//! and the file list arrives empty. The `FERROR_XFER` the sender framed is the
//! sole evidence of the failure: `read_a_msg` hands `MSG_ERROR_XFER` to
//! `rwrite` (`io.c:1660`), which sets `got_xfer_error` (`log.c:310-311`), and
//! `cleanup.c:217-218` lifts the zero exit to `RERR_PARTIAL`.
//!
//! These tests pin that chain at the receiver seam: identical wire streams that
//! differ only by the presence of the frame must differ only by the flag, with
//! `io_error` clear in both.

use std::io::Cursor;

use crate::reader::ServerReader;
use crate::transfer_state::TransferPhase;

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

/// The text upstream 3.4.4 frames for `rsync://host/data/nope`, captured from
/// a real daemon pull.
const LINK_STAT_TEXT: &[u8] =
    b"rsync: [sender] link_stat \"nope\" (in data) failed: No such file or directory (2)\n";

/// Drives a client receiver over a multiplexed stream carrying an empty file
/// list, optionally preceded by one `MSG_ERROR_XFER` frame, and returns the
/// resulting `(got_xfer_error, io_error)`.
fn empty_flist_run(with_error_xfer: bool) -> (bool, i32) {
    let mut wire = Vec::new();
    if with_error_xfer {
        protocol::send_msg(&mut wire, protocol::MessageCode::ErrorXfer, LINK_STAT_TEXT).unwrap();
    }
    // Protocol 32 file list: a bare 0x00 end marker and nothing else. No
    // MSG_IO_ERROR frame, exactly as upstream's ENOENT arm leaves it.
    protocol::send_msg(&mut wire, protocol::MessageCode::Data, &[0x00]).unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.connection.client_mode = true;
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.pipeline
        .advance_through(TransferPhase::DeltaTransfer)
        .unwrap();

    let mut reader = ServerReader::new_plain(Cursor::new(wire))
        .activate_multiplex()
        .unwrap();
    let count = ctx.receive_file_list(&mut reader).unwrap();
    assert_eq!(count, 0, "a missing source argument yields an empty list");

    let mut writer: Vec<u8> = Vec::new();
    let stats = ctx
        .finish_empty_client_flist(&mut reader, &mut writer)
        .unwrap();
    (stats.got_xfer_error, stats.io_error)
}

/// upstream: `io.c:1660` -> `log.c:310-311`. Receipt of the frame, not any
/// `io_error` bit, is what must make the run report a partial transfer.
#[test]
fn error_xfer_frame_on_an_empty_flist_sets_got_xfer_error() {
    let (got_xfer_error, io_error) = empty_flist_run(true);
    assert!(
        got_xfer_error,
        "MSG_ERROR_XFER must set got_xfer_error so the run exits 23"
    );
    assert_eq!(
        io_error, 0,
        "upstream flist.c:2431 withholds IOERR_GENERAL for ENOENT, so the exit \
         code must not come from an io_error bit"
    );
}

/// The control: the same empty list without the frame is a legitimately empty
/// transfer (every source filtered out), which upstream exits 0 on.
#[test]
fn empty_flist_without_an_error_xfer_frame_leaves_got_xfer_error_clear() {
    let (got_xfer_error, io_error) = empty_flist_run(false);
    assert!(
        !got_xfer_error,
        "a clean empty list must not be reported as a partial transfer"
    );
    assert_eq!(io_error, 0, "no error was reported on the wire");
}
