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

/// Sends the full values for entries marked as TODO.
///
/// # Wire Format
///
/// ```text
/// For each TODO entry:
///   length : varint
///   value  : bytes[length]
/// ```
pub fn send_xattr_values<W: Write>(writer: &mut W, list: &XattrList) -> io::Result<()> {
    for entry in list.iter() {
        if entry.state().needs_send() {
            write_varint(writer, entry.datum_len() as i32)?;
            writer.write_all(entry.datum())?;
        }
    }
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
