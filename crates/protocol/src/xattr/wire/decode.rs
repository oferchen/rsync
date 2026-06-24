//! Wire protocol decoding for extended attributes.
//!
//! Implements the receive-side functions for xattr data exchange between
//! rsync peers, including full-value and abbreviated (checksum-only)
//! reception.
//!
//! # Upstream Reference
//!
//! - `xattrs.c` - `recv_xattr_request()`, `receive_xattr()`

use std::io::{self, Read};

use crate::varint::read_varint;
use crate::xattr::{
    MAX_FULL_DATUM, MAX_WIRE_XATTR_COUNT, MAX_WIRE_XATTR_NAME_LEN, MAX_WIRE_XATTR_VALUE_LEN,
    MAX_XATTR_DIGEST_LEN, XattrEntry, XattrList,
};

use super::encode::compute_xattr_checksum;
use super::types::{RecvXattrResult, XattrDefinition, XattrSet};

/// Reads a set of xattr name-value definitions from the wire.
///
/// Parses the literal xattr data block that follows an `ndx == 0` indicator
/// during file list transfer. Reads a count of entries, then for each entry
/// reads name length, datum length, name bytes (with NUL terminator), and
/// either the full value or a checksum for abbreviated entries.
///
/// Names are returned in wire format without translation. The caller is
/// responsible for applying `wire_to_local()` if needed.
///
/// # Wire Format
///
/// ```text
/// count      : varint  // number of xattr entries
/// For each entry:
///   name_len  : varint  // includes trailing NUL byte
///   datum_len : varint  // original value length on sender
///   name      : bytes[name_len]  // NUL-terminated
///   If datum_len > MAX_FULL_DATUM (32):
///     checksum : bytes[MAX_XATTR_DIGEST_LEN]  // 16-byte MD5 digest
///   Else:
///     value    : bytes[datum_len]
/// ```
///
/// # Errors
///
/// Returns an error if the stream is truncated, the count is negative,
/// a name is empty, or a name is missing its trailing NUL terminator.
///
/// # Upstream Reference
///
/// See `xattrs.c:receive_xattr()` lines 790-860 - the entry-reading loop
/// after `ndx == 0` and before `rsync_xal_store()`.
pub fn read_xattr_definitions<R: Read>(reader: &mut R) -> io::Result<XattrSet> {
    let count = read_varint(reader)?;
    if count < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("negative xattr count: {count}"),
        ));
    }
    let count = count as usize;
    if count > MAX_WIRE_XATTR_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("xattr count {count} exceeds maximum {MAX_WIRE_XATTR_COUNT}"),
        ));
    }

    let mut entries = Vec::with_capacity(count);

    for _ in 0..count {
        // upstream: name_len = read_varint(f); datum_len = read_varint(f)
        let name_len = read_varint(reader)? as usize;
        let datum_len = read_varint(reader)? as usize;

        if name_len > MAX_WIRE_XATTR_NAME_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("xattr name length {name_len} exceeds maximum {MAX_WIRE_XATTR_NAME_LEN}"),
            ));
        }
        if datum_len > MAX_WIRE_XATTR_VALUE_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "xattr value length {datum_len} exceeds maximum {MAX_WIRE_XATTR_VALUE_LEN}"
                ),
            ));
        }

        let mut name = vec![0u8; name_len];
        reader.read_exact(&mut name)?;

        // upstream: name_len < 1 || name[name_len-1] != '\0' -> out_of_memory("receive_xattr")
        if name.is_empty() || name[name_len - 1] != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid xattr name: missing trailing NUL",
            ));
        }

        // Strip the NUL terminator for internal storage
        name.truncate(name_len - 1);

        let abbreviated = datum_len > MAX_FULL_DATUM;
        let read_len = if abbreviated {
            MAX_XATTR_DIGEST_LEN
        } else {
            datum_len
        };

        let mut datum = vec![0u8; read_len];
        reader.read_exact(&mut datum)?;

        entries.push(XattrDefinition {
            name,
            datum,
            datum_len,
            abbreviated,
        });
    }

    Ok(XattrSet { entries })
}

/// Receives xattr data from the wire during file list transfer.
///
/// Reads the `ndx` varint and dispatches:
/// - Non-negative `ndx` returns [`RecvXattrResult::CacheHit`] with the 0-based cache index.
/// - Negative `ndx` (wire value 0) reads inline literal entries and returns
///   [`RecvXattrResult::Literal`] with the parsed `XattrList`.
///
/// Literal entries with values exceeding [`MAX_FULL_DATUM`] are stored as
/// abbreviated checksums and must be resolved later via the request protocol.
///
/// # Upstream Reference
///
/// See `xattrs.c:receive_xattr()` - reads `ndx = read_varint(f)`, branches
/// on `ndx != 0` for cache hit vs literal data.
pub fn recv_xattr<R: Read>(reader: &mut R) -> io::Result<RecvXattrResult> {
    let ndx_plus_one = read_varint(reader)?;
    // upstream: xattrs.c:773-775 reads `int ndx = read_varint(f)` and rejects
    // out-of-range indices with an error. The wire value is an index + 1, so a
    // malicious peer can send i32::MIN, making `ndx_plus_one - 1` underflow and
    // panic under overflow-checks builds. Reject that edge with a protocol
    // error rather than panicking, mirroring upstream's index validation.
    let ndx = ndx_plus_one
        .checked_sub(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid xattr cache index"))?;

    if ndx >= 0 {
        return Ok(RecvXattrResult::CacheHit(ndx as u32));
    }

    let count = read_varint(reader)? as usize;
    if count > MAX_WIRE_XATTR_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("xattr count {count} exceeds maximum {MAX_WIRE_XATTR_COUNT}"),
        ));
    }
    let mut list = XattrList::new();

    for _ in 0..count {
        let name_len = read_varint(reader)? as usize;
        let datum_len = read_varint(reader)? as usize;

        if name_len > MAX_WIRE_XATTR_NAME_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("xattr name length {name_len} exceeds maximum {MAX_WIRE_XATTR_NAME_LEN}"),
            ));
        }
        if datum_len > MAX_WIRE_XATTR_VALUE_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "xattr value length {datum_len} exceeds maximum {MAX_WIRE_XATTR_VALUE_LEN}"
                ),
            ));
        }

        // upstream: name_len includes a trailing NUL terminator on the wire
        let mut name = vec![0u8; name_len];
        reader.read_exact(&mut name)?;
        if name.last() == Some(&0) {
            name.pop();
        }

        if datum_len > MAX_FULL_DATUM {
            let mut checksum = vec![0u8; MAX_XATTR_DIGEST_LEN];
            reader.read_exact(&mut checksum)?;
            list.push(XattrEntry::abbreviated(name, checksum, datum_len));
        } else {
            let mut value = vec![0u8; datum_len];
            reader.read_exact(&mut value)?;
            list.push(XattrEntry::new(name, value));
        }
    }

    Ok(RecvXattrResult::Literal(list))
}

/// Receives an xattr request and marks entries for sending.
///
/// Called by the sender to process the receiver's request for abbreviated values.
///
/// Wire format uses 1-based numbering. This function converts back to 0-based
/// indices for internal use.
///
/// # Wire Format
///
/// See [`super::encode::send_xattr_request`] for format description.
///
/// Returns the 0-based indices that were requested.
///
/// # Upstream Reference
///
/// See `xattrs.c:recv_xattr_request()` - reads 1-based `num` values with
/// delta encoding: `ndx = read_varint(f) + prior_req`.
pub fn recv_xattr_request<R: Read>(reader: &mut R, list: &mut XattrList) -> io::Result<Vec<usize>> {
    let mut indices = Vec::new();
    let mut prior_req = 0i32;

    loop {
        let rel = read_varint(reader)?;
        if rel == 0 {
            // upstream: 0 terminates the request list
            break;
        }

        // upstream: ndx = read_varint(f) + prior_req (1-based)
        let num = prior_req + rel;
        let idx = (num - 1) as usize;
        if idx < list.len() {
            list.mark_todo(idx);
            indices.push(idx);
        }
        prior_req = num;
    }

    Ok(indices)
}

/// Receives full values for abbreviated xattr entries from the sender.
///
/// Iterates over the list and for each entry in [`XattrState::Abbrev`](crate::xattr::XattrState::Abbrev)
/// state, reads the value length (varint) and full value bytes from the wire,
/// then updates the entry via [`XattrEntry::set_full_value`]. Entries already
/// in `Done` or `Todo` state are skipped.
///
/// Must be called after [`recv_xattr_request`] has been processed by the sender
/// and the sender has transmitted the requested values via [`send_xattr_values`](super::send_xattr_values).
///
/// # Upstream Reference
///
/// See `xattrs.c` - receiver reads full values for entries marked `XSTATE_ABBREV`.
pub fn recv_xattr_values<R: Read>(reader: &mut R, list: &mut XattrList) -> io::Result<()> {
    for entry in list.entries_mut() {
        if entry.state().needs_request() {
            let len = read_varint(reader)? as usize;
            if len > MAX_WIRE_XATTR_VALUE_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("xattr value length {len} exceeds maximum {MAX_WIRE_XATTR_VALUE_LEN}"),
                ));
            }
            let mut value = vec![0u8; len];
            reader.read_exact(&mut value)?;
            entry.set_full_value(value);
        }
    }
    Ok(())
}

/// Compares an abbreviated xattr checksum against a local value.
///
/// Computes the seeded MD5 digest of `local_value` using `checksum_seed` and
/// compares it byte-for-byte with `checksum`. Returns `true` if they match,
/// indicating the remote and local xattr values are identical and the full
/// value does not need to be transferred.
///
/// Returns `false` if `checksum` is not exactly [`MAX_XATTR_DIGEST_LEN`] bytes.
///
/// # Upstream Reference
///
/// Used during the abbreviation protocol in `xattrs.c` to determine which
/// abbreviated values the receiver needs to request from the sender.
#[must_use]
pub fn checksum_matches(checksum: &[u8], local_value: &[u8], checksum_seed: i32) -> bool {
    if checksum.len() != MAX_XATTR_DIGEST_LEN {
        return false;
    }
    let local_checksum = compute_xattr_checksum(local_value, checksum_seed);
    checksum == local_checksum
}

#[cfg(test)]
mod edg_panic_tests {
    use super::recv_xattr;
    use std::io;

    /// A malicious peer must not crash the parser by sending a cache index that
    /// underflows the `wire_value - 1` remap. upstream: xattrs.c:773-775 reads
    /// the index and rejects out-of-range values; the varint i32::MIN drives
    /// `ndx_plus_one - 1` past i32::MIN, so the hardened decode must return a
    /// clean InvalidData error rather than panicking under overflow-checks.
    #[test]
    fn recv_xattr_rejects_underflowing_cache_index() {
        // Varint of i32::MIN: leading tag 0xF0 (4 extra bytes) + LE 0x8000_0000.
        let wire = [0xF0u8, 0x00, 0x00, 0x00, 0x80];
        let err = recv_xattr(&mut &wire[..]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
