//! Wire protocol encoding for extended attributes.
//!
//! Implements the send-side functions for xattr data exchange between
//! rsync peers, including full-value and abbreviated (checksum-only)
//! transmission.
//!
//! # Upstream Reference
//!
//! - `xattrs.c` - `send_xattr_request()`, `send_xattr()`

use std::io::{self, Write};

use md5::{Digest, Md5};

use crate::varint::write_varint;
use crate::xattr::{MAX_FULL_DATUM, MAX_XATTR_DIGEST_LEN, XattrList};

/// Sends xattr data to the wire.
///
/// The `checksum_seed` is mixed into the hash for abbreviated xattr values,
/// matching upstream rsync's `sum_init(xattr_sum_nni, checksum_seed)` behavior.
///
/// # Wire Format
///
/// ```text
/// ndx + 1    : varint  // 0 means literal data follows, >0 is cache index
/// If ndx == 0 (literal data):
///   count    : varint  // number of xattr entries
///   For each entry:
///     name_len   : varint
///     datum_len  : varint  // original value length
///     name       : bytes[name_len]
///     If datum_len > MAX_FULL_DATUM:
///       checksum : bytes[MAX_XATTR_DIGEST_LEN]  // seeded hash of value
///     Else:
///       value    : bytes[datum_len]
/// ```
///
/// # Upstream Reference
///
/// See `xattrs.c` - abbreviated values use `sum_init(xattr_sum_nni, checksum_seed)`
/// to include the negotiated seed in the digest.
pub fn send_xattr<W: Write>(
    writer: &mut W,
    list: &XattrList,
    cached_index: Option<u32>,
    checksum_seed: i32,
) -> io::Result<()> {
    let ndx = cached_index.map(|i| i as i32).unwrap_or(-1);
    write_varint(writer, ndx + 1)?;

    if cached_index.is_none() {
        write_varint(writer, list.len() as i32)?;

        for entry in list.iter() {
            let name = entry.name();
            let datum_len = entry.datum_len();

            // upstream: name_len includes NUL terminator on the wire
            write_varint(writer, (name.len() + 1) as i32)?;
            write_varint(writer, datum_len as i32)?;
            writer.write_all(name)?;
            writer.write_all(&[0u8])?;

            if datum_len > MAX_FULL_DATUM {
                // upstream: sum_init(xattr_sum_nni, checksum_seed)
                let checksum = compute_xattr_checksum(entry.datum(), checksum_seed);
                writer.write_all(&checksum)?;
            } else {
                writer.write_all(entry.datum())?;
            }
        }
    }

    Ok(())
}

/// Sends a request for abbreviated xattr values.
///
/// Called by the receiver after determining which abbreviated values
/// are actually needed (differ from local values).
///
/// Callers provide 0-based indices. These are converted to 1-based on the
/// wire to match upstream rsync's `rxa->num` convention, where the first
/// entry is numbered 1. This avoids ambiguity with the 0 terminator.
///
/// # Wire Format
///
/// ```text
/// For each needed entry:
///   relative_num : varint  // 1-based num minus prior_req
/// terminator     : varint  // 0 to signal end of requests
/// ```
///
/// # Upstream Reference
///
/// See `xattrs.c:send_xattr_request()` - uses 1-based `rxa->num` with
/// delta encoding: `write_varint(f_out, rxa->num - prior_req)`.
pub fn send_xattr_request<W: Write>(writer: &mut W, indices: &[usize]) -> io::Result<()> {
    let mut prior_req = 0i32;

    for &idx in indices {
        // upstream: rxa->num is 1-based, convert 0-based index to 1-based
        let num = idx as i32 + 1;
        write_varint(writer, num - prior_req)?;
        prior_req = num;
    }

    // upstream: 0 terminates the request list
    write_varint(writer, 0)?;

    Ok(())
}

/// Sends the full values for entries whose state is marked as needing send.
///
/// Iterates over the xattr list and emits the full datum for every entry
/// whose `XattrState` reports `needs_send()` (the to-do state set by the
/// receiver after diffing the abbreviated value against its local copy).
/// Entries in any other state are skipped silently, preserving the relative
/// ordering of the on-wire stream.
///
/// # Wire Format
///
/// ```text
/// For each entry where state.needs_send() is true:
///   length : varint
///   value  : bytes[length]
/// ```
///
/// # Upstream Reference
///
/// See `xattrs.c:send_xattr()` - the second pass over the xattr list emits
/// values for entries flagged with the upstream pending-send state after
/// the receiver replies to the abbreviated digest exchange.
pub fn send_xattr_values<W: Write>(writer: &mut W, list: &XattrList) -> io::Result<()> {
    for entry in list.iter() {
        if entry.state().needs_send() {
            write_varint(writer, entry.datum_len() as i32)?;
            writer.write_all(entry.datum())?;
        }
    }
    Ok(())
}

/// Writes the sender-side response to the generator's xattr abbreviation request.
///
/// Mirrors upstream `xattrs.c:send_xattr_request()` when called by the
/// sender (`fname != NULL`, `f_out >= 0`). For every entry the generator
/// flagged via `XSTATE_TODO`, emits the delta-encoded 1-based `num`, the full
/// value length, and the value bytes. A trailing `0` varint terminates the
/// stream, matching upstream's `write_byte(f_out, 0)`.
///
/// The list passed in must have entries with `state().needs_send()` true for
/// the items the generator requested. After writing, those entries' states are
/// reset to [`XattrState::Done`](crate::xattr::XattrState::Done) to match
/// upstream's `rxa->datum[0] = XSTATE_DONE`.
///
/// # Wire Format
///
/// ```text
/// For each entry where state.needs_send() is true:
///   rel_num : varint  // num - prior_req (1-based, delta-encoded)
///   length  : varint
///   value   : bytes[length]
/// terminator: varint  // 0 signals end of stream
/// ```
///
/// # Upstream Reference
///
/// - `xattrs.c:623-675` - `send_xattr_request()` sender path (`fname != NULL`)
/// - `sender.c:192-196` - called from `write_ndx_and_attrs()` on the sender
///   when echoing iflags that include `ITEM_REPORT_XATTR`.
pub fn send_sender_xattr_response<W: Write>(
    writer: &mut W,
    list: &mut XattrList,
) -> io::Result<()> {
    use crate::xattr::XattrState;

    let mut prior_req = 0i32;
    for entry in list.entries_mut() {
        if !entry.state().needs_send() {
            continue;
        }
        let num = entry.num() as i32;
        // upstream: write_varint(f_out, rxa->num - prior_req)
        write_varint(writer, num - prior_req)?;
        prior_req = num;
        // upstream: write_varint(f_out, len); write_bigbuf(f_out, ptr, len)
        write_varint(writer, entry.datum_len() as i32)?;
        writer.write_all(entry.datum())?;
        // upstream: rxa->datum[0] = XSTATE_DONE after emission
        entry.set_state(XattrState::Done);
    }
    // upstream: write_byte(f_out, 0) - terminate the stream
    write_varint(writer, 0)?;
    Ok(())
}

/// Computes the seeded MD5 checksum for an xattr value.
///
/// Includes the `checksum_seed` in the hash to match upstream rsync's
/// `sum_init(xattr_sum_nni, checksum_seed)` + `sum_update()` + `sum_end()`
/// pattern. The seed bytes are hashed before the data.
///
/// # Upstream Reference
///
/// See `xattrs.c` - large xattr values are abbreviated using a seeded hash.
pub(super) fn compute_xattr_checksum(
    data: &[u8],
    checksum_seed: i32,
) -> [u8; MAX_XATTR_DIGEST_LEN] {
    let mut hasher = Md5::new();
    // upstream: sum_init() feeds the seed into the hash first
    hasher.update(checksum_seed.to_le_bytes());
    hasher.update(data);
    let result = hasher.finalize();
    let mut checksum = [0u8; MAX_XATTR_DIGEST_LEN];
    checksum.copy_from_slice(&result);
    checksum
}
