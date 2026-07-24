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
/// Abbreviated values (larger than `MAX_FULL_DATUM`) are replaced by the
/// checksum computed via `compute_xattr_checksum`. For the MD5 default of
/// protocol 30-32 this digest is unseeded, matching upstream's
/// `sum_init(xattr_sum_nni, checksum_seed)` whose `CSUM_MD5` path ignores the
/// seed (`checksum.c:588`).
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
///       checksum : bytes[MAX_XATTR_DIGEST_LEN]  // unseeded MD5 of value
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

/// Computes the abbreviation checksum for a large xattr value.
///
/// Upstream abbreviates values larger than `MAX_FULL_DATUM` with
/// `sum_init(xattr_sum_nni, checksum_seed)` + `sum_update(value)` +
/// `sum_end()`. For protocol versions 30-32 the negotiated `xattr_sum_nni`
/// is always MD5 (`compat.c:824` hardcodes `parse_csum_name(NULL, 0)`, which
/// returns md5 for protocol >= 30), and the streaming `sum_init()` path for
/// `CSUM_MD5` does `md5_begin()` only - it does NOT fold `checksum_seed` into
/// the digest. Only the MD4-family cases feed the seed via
/// `SIVAL(s, 0, seed); sum_update(s, 4)`. The result is therefore the plain
/// MD5 of the value bytes, and it must be computed unseeded to interoperate
/// with upstream: a seeded digest would never match the receiver's locally
/// computed abbreviation, forcing a redundant full-value transfer on every
/// large xattr.
///
/// `checksum_seed` is retained to mirror upstream's `sum_init(nni, seed)`
/// signature and to leave room for a future negotiated MD4-family algorithm;
/// the MD5 default deliberately ignores it, exactly as `sum_init()` does.
///
/// # Upstream Reference
///
/// - `xattrs.c:275-281` - `sum_init(xattr_sum_nni, checksum_seed)` over the datum.
/// - `checksum.c:588-597` - `sum_init()` `CSUM_MD5` case: `md5_begin()`, no seed.
/// - `compat.c:824-825` - `xattr_sum_nni` / `xattr_sum_len` fixed to md5 (16 bytes).
pub(crate) fn compute_xattr_checksum(
    data: &[u8],
    checksum_seed: i32,
) -> [u8; MAX_XATTR_DIGEST_LEN] {
    // upstream: the CSUM_MD5 branch of sum_init() ignores the seed; only the
    // MD4-family branches fold it in. Keep the parameter for signature parity.
    let _ = checksum_seed;
    let mut hasher = Md5::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut checksum = [0u8; MAX_XATTR_DIGEST_LEN];
    checksum.copy_from_slice(&result);
    checksum
}
