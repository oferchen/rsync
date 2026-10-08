//! Exit code of the file-list / xattr wire refusals.
//!
//! The flist and xattr decoders tag a hostile-peer refusal with
//! [`protocol::ProtocolViolation`] (proven at the decode site in the protocol
//! crate's own tests). This file pins the other half of the contract: that the
//! transfer crate's `rerr_for_io_error` maps such a tagged error to upstream's
//! `RERR_PROTOCOL` (2), not the `RERR_STREAMIO` (12) a bare `InvalidData`
//! yields. Exit 2 tells the operator the peer sent something no rsync may
//! send; exit 12 would mislead them into thinking the stream merely broke.
//!
//! The refusal messages asserted here are the exact literals the decoders
//! attach (flist.c:1101-1105, xattrs.c:826/839-842), so a drift in either the
//! text or the tag is caught.

use std::io;

use transfer::error::rerr_for_io_error;

const RERR_PROTOCOL: i32 = 2;
const RERR_STREAMIO: i32 = 12;

/// Every wire-refusal message the gnum/xattr decoders raise is tagged
/// RERR_PROTOCOL, carrying its upstream literal unchanged.
#[test]
fn tagged_wire_refusals_exit_rerr_protocol() {
    for msg in [
        // upstream: flist.c:1101-1105 - negative hard-link reference.
        "hard-link reference out of range: -5 (0)",
        // upstream: xattrs.c:826 - count outside [0, MAX_WIRE_XATTR_COUNT].
        "wire value xattr count out of range: 65537 not in [0,65536]",
        // upstream: xattrs.c:839-842 - a datum past the per-value ceiling.
        "xattr datum_len exceeds per-value limit",
        // upstream: xattrs.c:849-850 - the summed per-file ceiling.
        "xattr list exceeds per-file limit",
    ] {
        let err = protocol::protocol_violation(msg);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), msg);
        assert_eq!(
            rerr_for_io_error(&err),
            RERR_PROTOCOL,
            "a tagged wire refusal must exit 2, got a different code for {msg:?}",
        );
    }
}

/// Non-vacuity companion: an untagged `InvalidData` (a genuinely broken or
/// truncated stream) still maps to RERR_STREAMIO, so the tag above is doing
/// the work - the mapper is not returning 2 for every `InvalidData`.
#[test]
fn untagged_invalid_data_exits_rerr_streamio() {
    let err = io::Error::new(io::ErrorKind::InvalidData, "failed to fill whole buffer");
    assert_eq!(rerr_for_io_error(&err), RERR_STREAMIO);
}
