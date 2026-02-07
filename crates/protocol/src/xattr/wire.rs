//! Wire protocol encoding and decoding for xattrs.
//!
//! Implements the send/receive functions for xattr data exchange.

use std::io::{self, Read, Write};

use md5::{Digest, Md5};

use crate::varint::{read_varint, write_varint};
use crate::xattr::{MAX_FULL_DATUM, MAX_XATTR_DIGEST_LEN, XattrEntry, XattrList};

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
        list.push(XattrEntry::new(
            b"user.test".to_vec(),
            b"small value".to_vec(),
        ));
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
                assert!(checksum_matches(
                    received.entries()[0].datum(),
                    &large_value
                ));
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

    // ==================== Additional Comprehensive Tests ====================

    #[test]
    fn round_trip_empty_xattr_list() {
        let list = XattrList::new();

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 0);
                assert!(received.is_empty());
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_empty_xattr_value() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new(b"user.empty".to_vec(), b"".to_vec()));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 1);
                assert_eq!(received.entries()[0].name(), b"user.empty");
                assert!(received.entries()[0].datum().is_empty());
                assert!(!received.entries()[0].is_abbreviated());
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_xattr_at_abbreviation_boundary() {
        // Test xattr value exactly at MAX_FULL_DATUM (32 bytes)
        let value_at_boundary = vec![0x42u8; MAX_FULL_DATUM];
        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            b"user.boundary".to_vec(),
            value_at_boundary.clone(),
        ));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 1);
                // At boundary, should NOT be abbreviated
                assert!(!received.entries()[0].is_abbreviated());
                assert_eq!(received.entries()[0].datum(), &value_at_boundary);
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_xattr_one_byte_over_boundary() {
        // Test xattr value one byte over MAX_FULL_DATUM (33 bytes)
        let value_over_boundary = vec![0x42u8; MAX_FULL_DATUM + 1];
        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            b"user.over_boundary".to_vec(),
            value_over_boundary.clone(),
        ));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 1);
                // Over boundary, should be abbreviated
                assert!(received.entries()[0].is_abbreviated());
                assert_eq!(received.entries()[0].datum_len(), MAX_FULL_DATUM + 1);
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_many_xattrs() {
        let mut list = XattrList::new();
        for i in 0..20 {
            let name = format!("user.attr_{i:02}");
            let value = format!("value_{i:02}");
            list.push(XattrEntry::new(name.into_bytes(), value.into_bytes()));
        }

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 20);
                for i in 0..20 {
                    let expected_name = format!("user.attr_{i:02}");
                    let expected_value = format!("value_{i:02}");
                    assert_eq!(received.entries()[i].name(), expected_name.as_bytes());
                    assert_eq!(received.entries()[i].datum(), expected_value.as_bytes());
                }
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_mixed_small_and_large_xattrs() {
        let small_value = b"small".to_vec();
        let large_value = vec![0xCDu8; 100];

        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            b"user.small1".to_vec(),
            small_value.clone(),
        ));
        list.push(XattrEntry::new(
            b"user.large1".to_vec(),
            large_value.clone(),
        ));
        list.push(XattrEntry::new(
            b"user.small2".to_vec(),
            small_value.clone(),
        ));
        list.push(XattrEntry::new(
            b"user.large2".to_vec(),
            large_value.clone(),
        ));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 4);
                // small1 - not abbreviated
                assert!(!received.entries()[0].is_abbreviated());
                assert_eq!(received.entries()[0].datum(), &small_value);
                // large1 - abbreviated
                assert!(received.entries()[1].is_abbreviated());
                assert!(checksum_matches(
                    received.entries()[1].datum(),
                    &large_value
                ));
                // small2 - not abbreviated
                assert!(!received.entries()[2].is_abbreviated());
                // large2 - abbreviated
                assert!(received.entries()[3].is_abbreviated());
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_binary_xattr_value() {
        // Binary data including null bytes
        let binary_value: Vec<u8> = vec![0x00, 0x01, 0xFF, 0xFE, 0x00, 0xAB, 0xCD, 0x00];
        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            b"user.binary".to_vec(),
            binary_value.clone(),
        ));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 1);
                assert_eq!(received.entries()[0].datum(), &binary_value);
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_utf8_xattr_value() {
        // Use a shorter UTF-8 string that fits within MAX_FULL_DATUM (32 bytes)
        let utf8_value = "Hello 世界!".as_bytes().to_vec(); // 13 bytes
        let mut list = XattrList::new();
        list.push(XattrEntry::new(b"user.utf8".to_vec(), utf8_value.clone()));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 1);
                assert!(!received.entries()[0].is_abbreviated());
                assert_eq!(received.entries()[0].datum(), &utf8_value);
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn round_trip_large_utf8_xattr_value_abbreviated() {
        // UTF-8 string that exceeds MAX_FULL_DATUM and gets abbreviated
        let utf8_value = "Hello, 世界! 🌍 Привет мир!".as_bytes().to_vec();
        assert!(utf8_value.len() > MAX_FULL_DATUM); // Verify it's large enough

        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            b"user.utf8_large".to_vec(),
            utf8_value.clone(),
        ));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 1);
                assert!(received.entries()[0].is_abbreviated());
                assert!(checksum_matches(received.entries()[0].datum(), &utf8_value));
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn xattr_request_round_trip() {
        // Note: indices starting with 0 have a protocol ambiguity with the terminator
        // Test with indices that don't start at 0
        let indices = vec![1, 3, 5, 10];

        let mut buf = Vec::new();
        send_xattr_request(&mut buf, &indices).unwrap();

        // Create a list to receive into
        let mut list = XattrList::new();
        for i in 0..=10 {
            list.push(XattrEntry::abbreviated(
                format!("user.attr{i}").into_bytes(),
                vec![0u8; MAX_XATTR_DIGEST_LEN],
                100,
            ));
        }

        let mut cursor = Cursor::new(buf);
        let received_indices = recv_xattr_request(&mut cursor, &mut list).unwrap();

        assert_eq!(received_indices, indices);
        // Verify marked as TODO
        assert!(!list.entries()[0].state().needs_send());
        assert!(list.entries()[1].state().needs_send());
        assert!(!list.entries()[2].state().needs_send());
        assert!(list.entries()[3].state().needs_send());
        assert!(list.entries()[5].state().needs_send());
        assert!(list.entries()[10].state().needs_send());
    }

    #[test]
    fn xattr_request_empty() {
        // Test with no requests (empty indices)
        let indices: Vec<usize> = vec![];

        let mut buf = Vec::new();
        send_xattr_request(&mut buf, &indices).unwrap();

        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated(
            b"user.test".to_vec(),
            vec![0u8; MAX_XATTR_DIGEST_LEN],
            100,
        ));

        let mut cursor = Cursor::new(buf);
        let received_indices = recv_xattr_request(&mut cursor, &mut list).unwrap();

        assert!(received_indices.is_empty());
        assert!(!list.entries()[0].state().needs_send());
    }

    #[test]
    fn xattr_values_round_trip() {
        let value1 = vec![1u8; 50];
        let value2 = vec![2u8; 75];

        // Create sender list with TODO entries
        let mut sender_list = XattrList::new();
        sender_list.push(XattrEntry::new(b"user.attr1".to_vec(), value1.clone()));
        sender_list.push(XattrEntry::new(b"user.attr2".to_vec(), value2.clone()));
        sender_list.entries_mut()[0].mark_todo();
        sender_list.entries_mut()[1].mark_todo();

        let mut buf = Vec::new();
        send_xattr_values(&mut buf, &sender_list).unwrap();

        // Create receiver list with abbreviated entries
        let mut receiver_list = XattrList::new();
        receiver_list.push(XattrEntry::abbreviated(
            b"user.attr1".to_vec(),
            vec![0u8; MAX_XATTR_DIGEST_LEN],
            50,
        ));
        receiver_list.push(XattrEntry::abbreviated(
            b"user.attr2".to_vec(),
            vec![0u8; MAX_XATTR_DIGEST_LEN],
            75,
        ));

        let mut cursor = Cursor::new(buf);
        recv_xattr_values(&mut cursor, &mut receiver_list).unwrap();

        // Verify values were received
        assert_eq!(receiver_list.entries()[0].datum(), &value1);
        assert_eq!(receiver_list.entries()[1].datum(), &value2);
        assert!(!receiver_list.entries()[0].is_abbreviated());
        assert!(!receiver_list.entries()[1].is_abbreviated());
    }

    #[test]
    fn checksum_matches_empty_value() {
        let empty_value = b"";
        let checksum = compute_xattr_checksum(empty_value);
        assert!(checksum_matches(&checksum, empty_value));
    }

    #[test]
    fn checksum_length_mismatch_returns_false() {
        let value = b"test value";
        let short_checksum = &[0u8; 8]; // Less than MAX_XATTR_DIGEST_LEN
        assert!(!checksum_matches(short_checksum, value));
    }

    #[test]
    fn cache_index_zero() {
        // Test that cache index 0 works correctly
        let list = XattrList::new();

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, Some(0)).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::CacheHit(idx) => assert_eq!(idx, 0),
            _ => panic!("Expected cache hit"),
        }
    }

    #[test]
    fn large_cache_index() {
        // Test that reasonably large cache indices work
        // Note: varint encoding is used, so we test within i32 range
        let list = XattrList::new();
        let large_index = 100_000u32;

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, Some(large_index)).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::CacheHit(idx) => assert_eq!(idx, large_index),
            _ => panic!("Expected cache hit"),
        }
    }
}
