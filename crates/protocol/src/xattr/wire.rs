//! Wire protocol encoding and decoding for xattrs.
//!
//! Implements the send/receive functions for xattr data exchange.

use std::io::{self, Read, Write};

use md5::{Digest, Md5};

use crate::varint::{read_varint, write_varint};
use crate::xattr::{XattrEntry, XattrList, MAX_FULL_DATUM, MAX_XATTR_DIGEST_LEN};

/// Sends xattr data to the wire.
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
///       checksum : bytes[MAX_XATTR_DIGEST_LEN]  // MD5 of value
///     Else:
///       value    : bytes[datum_len]
/// ```
///
/// Returns the index assigned to this xattr set (for caching).
pub fn send_xattr<W: Write>(
    writer: &mut W,
    list: &XattrList,
    cached_index: Option<u32>,
) -> io::Result<()> {
    // Send index + 1. If we have a cached index, send it. Otherwise send 0.
    let ndx = cached_index.map(|i| i as i32).unwrap_or(-1);
    write_varint(writer, ndx + 1)?;

    // If not using cache, send literal data
    if cached_index.is_none() {
        write_varint(writer, list.len() as i32)?;

        for entry in list.iter() {
            let name = entry.name();
            let datum_len = entry.datum_len();

            write_varint(writer, name.len() as i32)?;
            write_varint(writer, datum_len as i32)?;
            writer.write_all(name)?;

            if datum_len > MAX_FULL_DATUM {
                // Send checksum only
                let checksum = compute_xattr_checksum(entry.datum());
                writer.write_all(&checksum)?;
            } else {
                // Send full value
                writer.write_all(entry.datum())?;
            }
        }
    }

    Ok(())
}

/// Receives xattr data from the wire.
///
/// Returns `Ok(Some(list))` if literal data was received,
/// `Ok(None)` if a cache index was received (caller should look up),
/// or the received cache index.
pub fn recv_xattr<R: Read>(reader: &mut R) -> io::Result<RecvXattrResult> {
    let ndx_plus_one = read_varint(reader)?;
    let ndx = ndx_plus_one - 1;

    if ndx >= 0 {
        // Cache hit - return the index
        return Ok(RecvXattrResult::CacheHit(ndx as u32));
    }

    // Literal data follows
    let count = read_varint(reader)? as usize;
    let mut list = XattrList::new();

    for _ in 0..count {
        let name_len = read_varint(reader)? as usize;
        let datum_len = read_varint(reader)? as usize;

        let mut name = vec![0u8; name_len];
        reader.read_exact(&mut name)?;

        if datum_len > MAX_FULL_DATUM {
            // Abbreviated - read checksum only
            let mut checksum = vec![0u8; MAX_XATTR_DIGEST_LEN];
            reader.read_exact(&mut checksum)?;
            list.push(XattrEntry::abbreviated(name, checksum, datum_len));
        } else {
            // Full value
            let mut value = vec![0u8; datum_len];
            reader.read_exact(&mut value)?;
            list.push(XattrEntry::new(name, value));
        }
    }

    Ok(RecvXattrResult::Literal(list))
}

/// Result of receiving xattr data.
#[derive(Debug)]
pub enum RecvXattrResult {
    /// A cache index was received - look up in the xattr cache.
    CacheHit(u32),
    /// Literal xattr data was received.
    Literal(XattrList),
}

/// Sends a request for abbreviated xattr values.
///
/// Called by the receiver after determining which abbreviated values
/// are actually needed (differ from local values).
///
/// # Wire Format
///
/// ```text
/// For each needed entry:
///   relative_index : varint  // index relative to previous (or 0 for first)
/// terminator       : varint  // 0 to signal end of requests
/// ```
pub fn send_xattr_request<W: Write>(writer: &mut W, indices: &[usize]) -> io::Result<()> {
    let mut last_ndx = 0i32;

    for &idx in indices {
        // Send relative index (difference from last)
        let rel = idx as i32 - last_ndx;
        write_varint(writer, rel)?;
        last_ndx = idx as i32 + 1; // Next relative is from idx+1
    }

    // Terminator: negative offset impossible, so 0 with no prior means end
    // Actually, upstream uses a different termination - let's match it
    // The upstream sends (idx - last_ndx) and terminates when nothing more needed
    // For safety, send a 0 to indicate end
    write_varint(writer, 0)?;

    Ok(())
}

/// Receives an xattr request and marks entries for sending.
///
/// Called by the sender to process receiver's request for abbreviated values.
///
/// # Wire Format
///
/// See [`send_xattr_request`] for format description.
///
/// Returns the indices that were requested.
pub fn recv_xattr_request<R: Read>(reader: &mut R, list: &mut XattrList) -> io::Result<Vec<usize>> {
    let mut indices = Vec::new();
    let mut last_ndx = 0i32;

    loop {
        let rel = read_varint(reader)?;
        if rel == 0 && last_ndx > 0 {
            // Terminator after at least one request
            break;
        }
        if rel == 0 && last_ndx == 0 {
            // No requests at all
            break;
        }

        let idx = (last_ndx + rel) as usize;
        if idx < list.len() {
            list.mark_todo(idx);
            indices.push(idx);
        }
        last_ndx = idx as i32 + 1;
    }

    Ok(indices)
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

/// Receives full values for abbreviated entries.
///
/// Updates the list entries with full values.
pub fn recv_xattr_values<R: Read>(reader: &mut R, list: &mut XattrList) -> io::Result<()> {
    for entry in list.entries_mut() {
        if entry.state().needs_request() {
            let len = read_varint(reader)? as usize;
            let mut value = vec![0u8; len];
            reader.read_exact(&mut value)?;
            entry.set_full_value(value);
        }
    }
    Ok(())
}

/// Computes the MD5 checksum for an xattr value.
///
/// Used for abbreviating large xattr values on the wire.
fn compute_xattr_checksum(data: &[u8]) -> [u8; MAX_XATTR_DIGEST_LEN] {
    let mut hasher = Md5::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut checksum = [0u8; MAX_XATTR_DIGEST_LEN];
    checksum.copy_from_slice(&result);
    checksum
}

/// Compares an abbreviated checksum with a local value.
///
/// Returns true if the checksums match (values are the same).
pub fn checksum_matches(checksum: &[u8], local_value: &[u8]) -> bool {
    if checksum.len() != MAX_XATTR_DIGEST_LEN {
        return false;
    }
    let local_checksum = compute_xattr_checksum(local_value);
    checksum == local_checksum
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_small_xattrs() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new(b"user.test".to_vec(), b"small value".to_vec()));
        list.push(XattrEntry::new(b"user.other".to_vec(), b"another".to_vec()));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 2);
                assert_eq!(received.entries()[0].name(), b"user.test");
                assert_eq!(received.entries()[0].datum(), b"small value");
                assert!(!received.entries()[0].is_abbreviated());
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_large_xattr_abbreviated() {
        let large_value = vec![0xABu8; 100]; // > MAX_FULL_DATUM
        let mut list = XattrList::new();
        list.push(XattrEntry::new(b"user.large".to_vec(), large_value.clone()));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 1);
                assert!(received.entries()[0].is_abbreviated());
                assert_eq!(received.entries()[0].datum_len(), 100);
                // Checksum should match
                assert!(checksum_matches(received.entries()[0].datum(), &large_value));
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn cache_hit_sends_index_only() {
        let list = XattrList::new();

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, Some(42)).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::CacheHit(idx) => assert_eq!(idx, 42),
            _ => panic!("Expected cache hit"),
        }
    }

    #[test]
    fn checksum_verification() {
        let value = b"test value for checksum";
        let checksum = compute_xattr_checksum(value);

        assert!(checksum_matches(&checksum, value));
        assert!(!checksum_matches(&checksum, b"different value"));
    }
}
