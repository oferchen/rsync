//! Wire protocol encoding and decoding for extended attributes.
//!
//! Implements the send/receive functions for xattr data exchange between
//! rsync peers. Supports both full-value and abbreviated (checksum-only)
//! transmission for bandwidth efficiency on large xattr values.
//!
//! # Upstream Reference
//!
//! - `xattrs.c` - `send_xattr_request()`, `recv_xattr_request()`, `send_xattr()`

use std::io::{self, Read, Write};

use md5::{Digest, Md5};

use crate::varint::{read_varint, write_varint};
use crate::xattr::{MAX_FULL_DATUM, MAX_XATTR_DIGEST_LEN, XattrEntry, XattrList};

/// A single parsed xattr name-value pair from the wire.
///
/// Represents one extended attribute as received during file list transfer,
/// before cache resolution or name translation. The name is in wire format
/// (e.g., without `user.` prefix on Linux) and has the trailing NUL stripped.
///
/// For values larger than `MAX_FULL_DATUM` (32 bytes), the datum contains
/// only a checksum and `is_abbreviated()` returns true.
///
/// # Upstream Reference
///
/// Corresponds to one iteration of the entry loop in `xattrs.c:receive_xattr()`.
#[derive(Debug, Clone)]
pub struct XattrDefinition {
    /// Attribute name in wire format (NUL-stripped).
    name: Vec<u8>,
    /// Full value bytes, or checksum bytes if abbreviated.
    datum: Vec<u8>,
    /// Original value length on the sender side.
    datum_len: usize,
    /// True when the value exceeds `MAX_FULL_DATUM` and only a checksum was sent.
    abbreviated: bool,
}

impl XattrDefinition {
    /// Returns the attribute name (wire format, NUL-stripped).
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Returns the attribute name as a lossy UTF-8 string.
    pub fn name_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }

    /// Returns the datum bytes - full value if small, checksum if abbreviated.
    pub fn datum(&self) -> &[u8] {
        &self.datum
    }

    /// Returns the original value length on the sender.
    ///
    /// For abbreviated entries this differs from `datum().len()`.
    pub const fn datum_len(&self) -> usize {
        self.datum_len
    }

    /// Returns true if this entry was abbreviated (checksum only, no full value).
    pub const fn is_abbreviated(&self) -> bool {
        self.abbreviated
    }

    /// Converts this definition into an `XattrEntry` for use with `XattrList`.
    pub fn into_entry(self) -> XattrEntry {
        if self.abbreviated {
            XattrEntry::abbreviated(self.name, self.datum, self.datum_len)
        } else {
            XattrEntry::new(self.name, self.datum)
        }
    }
}

/// A parsed set of xattr name-value pairs from the wire.
///
/// Contains zero or more `XattrDefinition` entries as read from a single
/// literal xattr block during file list transfer. Names are in wire format
/// and have not been translated to local platform conventions.
///
/// # Upstream Reference
///
/// Corresponds to the literal-data branch of `xattrs.c:receive_xattr()`,
/// after reading `ndx == 0` and before `rsync_xal_store()`.
#[derive(Debug, Clone, Default)]
pub struct XattrSet {
    /// Parsed entries in wire order.
    entries: Vec<XattrDefinition>,
}

impl XattrSet {
    /// Creates an empty xattr set.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Returns the number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns a slice of all entries.
    pub fn entries(&self) -> &[XattrDefinition] {
        &self.entries
    }

    /// Consumes the set and returns the entries as a vector.
    pub fn into_entries(self) -> Vec<XattrDefinition> {
        self.entries
    }

    /// Converts this set into an `XattrList` for use with the cache and
    /// abbreviation protocol.
    pub fn into_xattr_list(self) -> XattrList {
        let entries: Vec<XattrEntry> = self
            .entries
            .into_iter()
            .map(XattrDefinition::into_entry)
            .collect();
        XattrList::with_entries(entries)
    }

    /// Returns an iterator over the entries.
    pub fn iter(&self) -> impl Iterator<Item = &XattrDefinition> {
        self.entries.iter()
    }
}

impl<'a> IntoIterator for &'a XattrSet {
    type Item = &'a XattrDefinition;
    type IntoIter = std::slice::Iter<'a, XattrDefinition>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl IntoIterator for XattrSet {
    type Item = XattrDefinition;
    type IntoIter = std::vec::IntoIter<XattrDefinition>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

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
#[must_use]
pub fn read_xattr_definitions<R: Read>(reader: &mut R) -> io::Result<XattrSet> {
    let count = read_varint(reader)?;
    if count < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("negative xattr count: {count}"),
        ));
    }
    let count = count as usize;

    let mut entries = Vec::with_capacity(count);

    for _ in 0..count {
        // upstream: name_len = read_varint(f); datum_len = read_varint(f)
        let name_len = read_varint(reader)? as usize;
        let datum_len = read_varint(reader)? as usize;

        // Read name bytes (includes NUL terminator from upstream)
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
                // upstream: sum_init(xattr_sum_nni, checksum_seed)
                let checksum = compute_xattr_checksum(entry.datum(), checksum_seed);
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
#[must_use]
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
#[must_use]
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

/// Receives an xattr request and marks entries for sending.
///
/// Called by the sender to process receiver's request for abbreviated values.
///
/// Wire format uses 1-based numbering. This function converts back to 0-based
/// indices for internal use.
///
/// # Wire Format
///
/// See [`send_xattr_request`] for format description.
///
/// Returns the 0-based indices that were requested.
///
/// # Upstream Reference
///
/// See `xattrs.c:recv_xattr_request()` - reads 1-based `num` values with
/// delta encoding: `ndx = read_varint(f) + prior_req`.
#[must_use]
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
        // Convert 1-based wire num to 0-based index
        let idx = (num - 1) as usize;
        if idx < list.len() {
            list.mark_todo(idx);
            indices.push(idx);
        }
        prior_req = num;
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
#[must_use]
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
#[must_use]
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

/// Computes the seeded MD5 checksum for an xattr value.
///
/// Includes the `checksum_seed` in the hash to match upstream rsync's
/// `sum_init(xattr_sum_nni, checksum_seed)` + `sum_update()` + `sum_end()`
/// pattern. The seed bytes are hashed before the data.
///
/// # Upstream Reference
///
/// See `xattrs.c` - large xattr values are abbreviated using a seeded hash.
fn compute_xattr_checksum(data: &[u8], checksum_seed: i32) -> [u8; MAX_XATTR_DIGEST_LEN] {
    let mut hasher = Md5::new();
    // upstream: sum_init() feeds the seed into the hash first
    hasher.update(checksum_seed.to_le_bytes());
    hasher.update(data);
    let result = hasher.finalize();
    let mut checksum = [0u8; MAX_XATTR_DIGEST_LEN];
    checksum.copy_from_slice(&result);
    checksum
}

/// Compares an abbreviated checksum with a local value.
///
/// The `checksum_seed` must match the seed used when the checksum was computed.
///
/// Returns true if the checksums match (values are the same).
pub fn checksum_matches(checksum: &[u8], local_value: &[u8], checksum_seed: i32) -> bool {
    if checksum.len() != MAX_XATTR_DIGEST_LEN {
        return false;
    }
    let local_checksum = compute_xattr_checksum(local_value, checksum_seed);
    checksum == local_checksum
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use crate::varint::write_varint;

    /// Helper to write a literal xattr definition block (count + entries) in wire format.
    ///
    /// Names are NUL-terminated on the wire. Each entry has name_len (including NUL),
    /// datum_len, name bytes, and value or fake checksum bytes.
    fn write_definition_block(buf: &mut Vec<u8>, entries: &[(&[u8], &[u8])]) {
        write_varint(buf, entries.len() as i32).unwrap();
        for &(name, value) in entries {
            // name_len includes NUL terminator
            write_varint(buf, (name.len() + 1) as i32).unwrap();
            write_varint(buf, value.len() as i32).unwrap();
            buf.extend_from_slice(name);
            buf.push(0); // NUL terminator
            if value.len() > MAX_FULL_DATUM {
                // Abbreviated - write fake 16-byte checksum
                buf.extend_from_slice(&[0xAA; MAX_XATTR_DIGEST_LEN]);
            } else {
                buf.extend_from_slice(value);
            }
        }
    }

    #[test]
    fn read_definitions_empty() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn read_definitions_single_small_entry() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.test", b"hello")]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert_eq!(set.len(), 1);
        let entry = &set.entries()[0];
        assert_eq!(entry.name(), b"user.test");
        assert_eq!(entry.datum(), b"hello");
        assert_eq!(entry.datum_len(), 5);
        assert!(!entry.is_abbreviated());
    }

    #[test]
    fn read_definitions_multiple_entries() {
        let mut buf = Vec::new();
        write_definition_block(
            &mut buf,
            &[
                (b"user.alpha", b"value_a"),
                (b"user.beta", b"value_b"),
                (b"user.gamma", b"value_c"),
            ],
        );

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert_eq!(set.len(), 3);
        assert_eq!(set.entries()[0].name(), b"user.alpha");
        assert_eq!(set.entries()[0].datum(), b"value_a");
        assert_eq!(set.entries()[1].name(), b"user.beta");
        assert_eq!(set.entries()[1].datum(), b"value_b");
        assert_eq!(set.entries()[2].name(), b"user.gamma");
        assert_eq!(set.entries()[2].datum(), b"value_c");
    }

    #[test]
    fn read_definitions_abbreviated_large_value() {
        let large_value = vec![0xBB; 100]; // > MAX_FULL_DATUM
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.large", &large_value)]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert_eq!(set.len(), 1);
        let entry = &set.entries()[0];
        assert_eq!(entry.name(), b"user.large");
        assert!(entry.is_abbreviated());
        assert_eq!(entry.datum_len(), 100);
        assert_eq!(entry.datum().len(), MAX_XATTR_DIGEST_LEN);
    }

    #[test]
    fn read_definitions_mixed_small_and_large() {
        let large_value = vec![0xCC; 64];
        let mut buf = Vec::new();
        write_definition_block(
            &mut buf,
            &[
                (b"user.small", b"tiny"),
                (b"user.large", &large_value),
                (b"user.also_small", b"also tiny"),
            ],
        );

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert_eq!(set.len(), 3);
        assert!(!set.entries()[0].is_abbreviated());
        assert_eq!(set.entries()[0].datum(), b"tiny");
        assert!(set.entries()[1].is_abbreviated());
        assert_eq!(set.entries()[1].datum_len(), 64);
        assert!(!set.entries()[2].is_abbreviated());
        assert_eq!(set.entries()[2].datum(), b"also tiny");
    }

    #[test]
    fn read_definitions_empty_value() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.empty", b"")]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert_eq!(set.len(), 1);
        assert!(!set.entries()[0].is_abbreviated());
        assert!(set.entries()[0].datum().is_empty());
        assert_eq!(set.entries()[0].datum_len(), 0);
    }

    #[test]
    fn read_definitions_boundary_value() {
        // Exactly at MAX_FULL_DATUM - should NOT be abbreviated
        let boundary_value = vec![0x42u8; MAX_FULL_DATUM];
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.boundary", &boundary_value)]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert_eq!(set.len(), 1);
        assert!(!set.entries()[0].is_abbreviated());
        assert_eq!(set.entries()[0].datum(), &boundary_value);
    }

    #[test]
    fn read_definitions_one_over_boundary() {
        // One byte over MAX_FULL_DATUM - should be abbreviated
        let over_value = vec![0x42u8; MAX_FULL_DATUM + 1];
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.over", &over_value)]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert_eq!(set.len(), 1);
        assert!(set.entries()[0].is_abbreviated());
        assert_eq!(set.entries()[0].datum_len(), MAX_FULL_DATUM + 1);
    }

    #[test]
    fn read_definitions_binary_value() {
        let binary_value: Vec<u8> = vec![0x00, 0x01, 0xFF, 0xFE, 0x00];
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.bin", &binary_value)]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();

        assert_eq!(set.entries()[0].datum(), &binary_value);
    }

    #[test]
    fn read_definitions_negative_count_fails() {
        let mut buf = Vec::new();
        write_varint(&mut buf, -1).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = read_xattr_definitions(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("negative xattr count"));
    }

    #[test]
    fn read_definitions_missing_nul_fails() {
        let mut buf = Vec::new();
        // count = 1
        write_varint(&mut buf, 1).unwrap();
        // name_len = 4 (no NUL)
        write_varint(&mut buf, 4).unwrap();
        // datum_len = 1
        write_varint(&mut buf, 1).unwrap();
        // name without NUL terminator
        buf.extend_from_slice(b"test");
        // value
        buf.push(0x42);

        let mut cursor = Cursor::new(buf);
        let result = read_xattr_definitions(&mut cursor);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("NUL"));
    }

    #[test]
    fn read_definitions_empty_name_fails() {
        let mut buf = Vec::new();
        // count = 1
        write_varint(&mut buf, 1).unwrap();
        // name_len = 0 (empty)
        write_varint(&mut buf, 0).unwrap();
        // datum_len = 1
        write_varint(&mut buf, 1).unwrap();
        // value
        buf.push(0x42);

        let mut cursor = Cursor::new(buf);
        let result = read_xattr_definitions(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn definition_name_lossy() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.test", b"val")]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();
        assert_eq!(set.entries()[0].name_lossy(), "user.test");
    }

    #[test]
    fn xattr_set_into_xattr_list() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.a", b"val_a"), (b"user.b", b"val_b")]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();
        let list = set.into_xattr_list();

        assert_eq!(list.len(), 2);
        assert_eq!(list.entries()[0].name(), b"user.a");
        assert_eq!(list.entries()[0].datum(), b"val_a");
        assert_eq!(list.entries()[1].name(), b"user.b");
        assert_eq!(list.entries()[1].datum(), b"val_b");
    }

    #[test]
    fn xattr_set_into_xattr_list_with_abbreviated() {
        let large_value = vec![0xDD; 50];
        let mut buf = Vec::new();
        write_definition_block(
            &mut buf,
            &[(b"user.small", b"tiny"), (b"user.large", &large_value)],
        );

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();
        let list = set.into_xattr_list();

        assert_eq!(list.len(), 2);
        assert!(!list.entries()[0].is_abbreviated());
        assert!(list.entries()[1].is_abbreviated());
        assert_eq!(list.entries()[1].datum_len(), 50);
    }

    #[test]
    fn definition_into_entry_full() {
        let def = XattrDefinition {
            name: b"user.test".to_vec(),
            datum: b"value".to_vec(),
            datum_len: 5,
            abbreviated: false,
        };

        let entry = def.into_entry();
        assert_eq!(entry.name(), b"user.test");
        assert_eq!(entry.datum(), b"value");
        assert!(!entry.is_abbreviated());
    }

    #[test]
    fn definition_into_entry_abbreviated() {
        let def = XattrDefinition {
            name: b"user.large".to_vec(),
            datum: vec![0xAA; MAX_XATTR_DIGEST_LEN],
            datum_len: 100,
            abbreviated: true,
        };

        let entry = def.into_entry();
        assert_eq!(entry.name(), b"user.large");
        assert!(entry.is_abbreviated());
        assert_eq!(entry.datum_len(), 100);
        assert_eq!(entry.datum().len(), MAX_XATTR_DIGEST_LEN);
    }

    #[test]
    fn xattr_set_into_entries() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.x", b"x_val")]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();
        let entries = set.into_entries();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name(), b"user.x");
    }

    #[test]
    fn xattr_set_iter() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.a", b"1"), (b"user.b", b"2")]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();
        let names: Vec<&[u8]> = set.iter().map(|d| d.name()).collect();
        assert_eq!(names, vec![b"user.a".as_slice(), b"user.b".as_slice()]);
    }

    #[test]
    fn xattr_set_into_iterator() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.x", b"val")]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();
        let collected: Vec<XattrDefinition> = set.into_iter().collect();
        assert_eq!(collected.len(), 1);
    }

    #[test]
    fn xattr_set_ref_into_iterator() {
        let mut buf = Vec::new();
        write_definition_block(&mut buf, &[(b"user.x", b"val")]);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();
        let names: Vec<&[u8]> = (&set).into_iter().map(|d| d.name()).collect();
        assert_eq!(names, vec![b"user.x".as_slice()]);
    }

    #[test]
    fn read_definitions_many_entries() {
        let mut entries_data: Vec<(&[u8], Vec<u8>)> = Vec::new();
        for i in 0..20u8 {
            let name = format!("user.attr_{i:02}");
            let value = format!("value_{i:02}");
            entries_data.push((
                Box::leak(name.into_bytes().into_boxed_slice()),
                value.into_bytes(),
            ));
        }
        let refs: Vec<(&[u8], &[u8])> = entries_data
            .iter()
            .map(|(n, v)| (*n, v.as_slice()))
            .collect();

        let mut buf = Vec::new();
        write_definition_block(&mut buf, &refs);

        let mut cursor = Cursor::new(buf);
        let set = read_xattr_definitions(&mut cursor).unwrap();
        assert_eq!(set.len(), 20);
        assert_eq!(set.entries()[0].name(), b"user.attr_00");
        assert_eq!(set.entries()[19].name(), b"user.attr_19");
    }

    // ==================== Existing tests below ====================

    #[test]
    fn round_trip_small_xattrs() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            b"user.test".to_vec(),
            b"small value".to_vec(),
        ));
        list.push(XattrEntry::new(b"user.other".to_vec(), b"another".to_vec()));

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
                    &large_value,
                    0
                ));
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn cache_hit_sends_index_only() {
        let list = XattrList::new();

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, Some(42), 0).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::CacheHit(idx) => assert_eq!(idx, 42),
            _ => panic!("Expected cache hit"),
        }
    }

    #[test]
    fn checksum_verification() {
        let seed = 12345;
        let value = b"test value for checksum";
        let checksum = compute_xattr_checksum(value, seed);

        assert!(checksum_matches(&checksum, value, seed));
        assert!(!checksum_matches(&checksum, b"different value", seed));
    }

    #[test]
    fn checksum_seed_affects_result() {
        let value = b"same data different seeds";
        let checksum_a = compute_xattr_checksum(value, 100);
        let checksum_b = compute_xattr_checksum(value, 200);
        assert_ne!(checksum_a, checksum_b);
        assert!(checksum_matches(&checksum_a, value, 100));
        assert!(!checksum_matches(&checksum_a, value, 200));
    }

    // ==================== Additional Comprehensive Tests ====================

    #[test]
    fn round_trip_empty_xattr_list() {
        let list = XattrList::new();

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
                    &large_value,
                    0
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
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
        send_xattr(&mut buf, &list, None, 0).unwrap();

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
        send_xattr(&mut buf, &list, None, 0).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::Literal(received) => {
                assert_eq!(received.len(), 1);
                assert!(received.entries()[0].is_abbreviated());
                assert!(checksum_matches(
                    received.entries()[0].datum(),
                    &utf8_value,
                    0
                ));
            }
            _ => panic!("Expected literal data"),
        }
    }

    #[test]
    fn xattr_request_round_trip() {
        // 1-based wire encoding allows index 0 without ambiguity
        let indices = vec![0, 1, 3, 5, 10];

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
        assert!(list.entries()[0].state().needs_send());
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
        let checksum = compute_xattr_checksum(empty_value, 0);
        assert!(checksum_matches(&checksum, empty_value, 0));
    }

    #[test]
    fn checksum_length_mismatch_returns_false() {
        let value = b"test value";
        let short_checksum = &[0u8; 8]; // Less than MAX_XATTR_DIGEST_LEN
        assert!(!checksum_matches(short_checksum, value, 0));
    }

    #[test]
    fn cache_index_zero() {
        // Test that cache index 0 works correctly
        let list = XattrList::new();

        let mut buf = Vec::new();
        send_xattr(&mut buf, &list, Some(0), 0).unwrap();

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
        send_xattr(&mut buf, &list, Some(large_index), 0).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_xattr(&mut cursor).unwrap();

        match result {
            RecvXattrResult::CacheHit(idx) => assert_eq!(idx, large_index),
            _ => panic!("Expected cache hit"),
        }
    }
}
