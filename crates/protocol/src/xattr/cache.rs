//! Xattr cache for receiver-side file list processing.
//!
//! During file list reception, xattr sets are deduplicated using an index-based
//! cache. Each unique xattr set is stored once and referenced by index from
//! multiple file entries. This mirrors upstream rsync's `rsync_xal_l` list.
//!
//! After reading raw name-value pairs from the wire, names are translated from
//! wire format to local platform conventions using `wire_to_local()`. Entries
//! whose names cannot be stored locally (e.g., non-user namespace xattrs when
//! running as non-root on Linux) are filtered out. When name translation
//! changes the sort order, entries are re-sorted by name to maintain the
//! invariant that xattr lists are sorted alphabetically.
//!
//! # Upstream Reference
//!
//! - `xattrs.c:receive_xattr()` - reads index or literal data, stores in cache
//! - `xattrs.c:rsync_xal_store()` - adds xattr list to global cache

use std::io::{self, Read};

use crate::varint::read_varint;
use crate::xattr::prefix::wire_to_local;
use crate::xattr::{MAX_FULL_DATUM, MAX_XATTR_DIGEST_LEN, RSYNC_PREFIX, XattrEntry, XattrList};

/// Cache of received xattr sets, indexed for deduplication.
///
/// Mirrors upstream's `rsync_xal_l` item list. Each file entry references
/// an xattr set by index into this cache, avoiding duplicate storage of
/// identical xattr sets across multiple files.
#[derive(Debug, Default)]
pub struct XattrCache {
    /// Stored xattr lists, indexed by position.
    lists: Vec<XattrList>,
}

impl XattrCache {
    /// Creates an empty xattr cache.
    #[must_use]
    pub fn new() -> Self {
        Self { lists: Vec::new() }
    }

    /// Returns the number of cached xattr sets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lists.len()
    }

    /// Returns true if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lists.is_empty()
    }

    /// Retrieves a cached xattr list by index.
    pub fn get(&self, index: usize) -> Option<&XattrList> {
        self.lists.get(index)
    }

    /// Retrieves a mutable reference to a cached xattr list by index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut XattrList> {
        self.lists.get_mut(index)
    }

    /// Stores an xattr list in the cache and returns its index.
    #[must_use]
    pub fn store(&mut self, list: XattrList) -> u32 {
        let index = self.lists.len();
        self.lists.push(list);
        index as u32
    }

    /// Receives an xattr set from the wire during file list reading.
    ///
    /// Mirrors upstream `xattrs.c:receive_xattr()`. Reads a varint index:
    /// - If non-zero, the value minus one is a cache index referencing a
    ///   previously received xattr set.
    /// - If zero, literal xattr data follows: a count of entries, each with
    ///   name length, datum length, name bytes, and value or checksum bytes.
    ///
    /// After reading each entry, the name is translated from wire format to
    /// local platform conventions via `wire_to_local()`. Entries that cannot
    /// be stored locally are silently dropped. When name translation modifies
    /// names (e.g., adding `user.` prefix on Linux), the entry list is
    /// re-sorted by name to maintain upstream's sorted invariant.
    ///
    /// Returns the cache index assigned to this file's xattr set.
    ///
    /// # Wire Format
    ///
    /// ```text
    /// ndx : varint  // 0 = literal follows, >0 = cache index (ndx-1)
    /// If ndx == 0:
    ///   count      : varint
    ///   For each entry:
    ///     name_len  : varint  // includes NUL terminator
    ///     datum_len : varint  // original value length
    ///     name      : bytes[name_len]
    ///     If datum_len > MAX_FULL_DATUM:
    ///       checksum : bytes[MAX_XATTR_DIGEST_LEN]
    ///     Else:
    ///       value    : bytes[datum_len]
    /// ```
    ///
    /// # Arguments
    ///
    /// * `reader` - Wire protocol stream
    /// * `am_root` - Whether receiver has root privileges (affects namespace handling)
    /// * `preserve_xattrs` - Xattr preservation level (1 = normal, 2 = include rsync.% attrs)
    ///
    /// # Upstream Reference
    ///
    /// See `xattrs.c:receive_xattr()` lines 764-869.
    pub fn receive_xattr<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        am_root: bool,
        preserve_xattrs: u32,
    ) -> io::Result<u32> {
        // upstream: ndx = read_varint(f)
        let ndx = read_varint(reader)?;

        if ndx < 0 || (ndx as usize) > self.lists.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "xattr index {} out of range (cache size {})",
                    ndx,
                    self.lists.len()
                ),
            ));
        }

        // upstream: if (ndx != 0) { F_XATTR(file) = ndx - 1; return; }
        if ndx != 0 {
            return Ok((ndx - 1) as u32);
        }

        // Literal xattr data follows
        let count = read_varint(reader)?;
        if count < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("negative xattr count: {count}"),
            ));
        }
        let count = count as usize;

        let mut list = XattrList::new();
        let mut need_sort = !cfg!(target_os = "linux");

        for num in 1..=count {
            // upstream: name_len = read_varint(f); datum_len = read_varint(f)
            let name_len = read_varint(reader)? as usize;
            let datum_len = read_varint(reader)? as usize;

            // upstream: dget_len = datum_len > MAX_FULL_DATUM ? 1 + xattr_sum_len : datum_len
            let dget_len = if datum_len > MAX_FULL_DATUM {
                MAX_XATTR_DIGEST_LEN
            } else {
                datum_len
            };

            // Read name bytes (includes NUL terminator from upstream)
            let mut name = vec![0u8; name_len];
            reader.read_exact(&mut name)?;

            // upstream: name_len < 1 || name[name_len-1] != '\0' -> error
            if name.is_empty() || name[name_len - 1] != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid xattr name: missing trailing NUL",
                ));
            }

            // Strip the NUL terminator for internal storage
            name.truncate(name_len - 1);

            // Read value or checksum bytes from the wire before any filtering,
            // since we must consume the data regardless of whether we keep the entry.
            let datum_bytes = {
                let mut buf = vec![0u8; dget_len];
                reader.read_exact(&mut buf)?;
                buf
            };

            // upstream: xattrs.c:820-853 - translate wire name to local name
            let local_name = match wire_to_local(&name, am_root) {
                Some(n) => n,
                None => {
                    // Cannot store this xattr locally - skip it
                    continue;
                }
            };

            // upstream: xattrs.c:848-853 - skip rsync.%FOO internal attrs
            // unless preserve_xattrs >= 2 (double -X)
            if preserve_xattrs < 2 && is_rsync_internal_attr(&local_name) {
                continue;
            }

            // Track whether name translation changed a name, requiring re-sort.
            // upstream: xattrs.c:830 - need_sort = 1 on name prefix changes
            if !need_sort && local_name != name {
                need_sort = true;
            }

            if datum_len > MAX_FULL_DATUM {
                let mut entry = XattrEntry::abbreviated(local_name, datum_bytes, datum_len);
                entry.set_num(num as u32);
                list.push(entry);
            } else {
                let mut entry = XattrEntry::new(local_name, datum_bytes);
                entry.set_num(num as u32);
                list.push(entry);
            }
        }

        // upstream: xattrs.c:863-864 - sort by name when translations changed order
        if need_sort && list.len() > 1 {
            list.sort_by_name();
        }

        // upstream: ndx = rsync_xal_store(&temp_xattr)
        let stored_ndx = self.store(list);
        Ok(stored_ndx)
    }
}

/// Checks whether a local-format xattr name is an rsync internal attribute.
///
/// Internal attributes use the `rsync.%` prefix (or `user.rsync.%` on Linux).
/// These are only preserved when `preserve_xattrs >= 2` (double `-X`).
///
/// # Upstream Reference
///
/// See `xattrs.c:848-853` - `preserve_xattrs < 2 && name[RPRE_LEN] == '%'`
fn is_rsync_internal_attr(name: &[u8]) -> bool {
    let name_str = match std::str::from_utf8(name) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let rpre = RSYNC_PREFIX;
    if name_str.len() > rpre.len() {
        if let Some(rest) = name_str.strip_prefix(rpre) {
            return rest.starts_with('%');
        }
    }

    // On Linux, check user.rsync.% form
    #[cfg(target_os = "linux")]
    {
        let full_prefix = format!("user.{rpre}");
        if name_str.len() > full_prefix.len() {
            if let Some(rest) = name_str.strip_prefix(full_prefix.as_str()) {
                return rest.starts_with('%');
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::varint::write_varint;
    use crate::xattr::XattrState;
    use std::io::Cursor;

    /// Helper to write a literal xattr set to a buffer in wire format.
    ///
    /// Names are written as-is (wire format). The caller is responsible for
    /// using proper wire-format names (e.g., without `user.` prefix on Linux).
    fn write_literal_xattr(buf: &mut Vec<u8>, entries: &[(&[u8], &[u8])]) {
        // ndx = 0 means literal follows
        write_varint(buf, 0).unwrap();
        // count
        write_varint(buf, entries.len() as i32).unwrap();
        for &(name, value) in entries {
            // name_len includes NUL terminator
            write_varint(buf, (name.len() + 1) as i32).unwrap();
            // datum_len
            write_varint(buf, value.len() as i32).unwrap();
            // name bytes + NUL
            buf.extend_from_slice(name);
            buf.push(0);
            // value or checksum
            if value.len() > MAX_FULL_DATUM {
                // For test simplicity, write a fake 16-byte checksum
                buf.extend_from_slice(&[0xAA; MAX_XATTR_DIGEST_LEN]);
            } else {
                buf.extend_from_slice(value);
            }
        }
    }

    /// Helper to write a cache-hit reference.
    fn write_cache_hit(buf: &mut Vec<u8>, index: u32) {
        // ndx = index + 1 (non-zero means cache hit)
        write_varint(buf, (index + 1) as i32).unwrap();
    }

    /// Returns the expected local name after wire_to_local translation.
    ///
    /// On Linux, wire names get `user.` prepended. On other platforms, they
    /// pass through unchanged.
    fn expected_local_name(wire_name: &[u8]) -> Vec<u8> {
        #[cfg(target_os = "linux")]
        {
            let mut local = b"user.".to_vec();
            local.extend_from_slice(wire_name);
            local
        }
        #[cfg(not(target_os = "linux"))]
        {
            wire_name.to_vec()
        }
    }

    #[test]
    fn receive_literal_xattr_set() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        write_literal_xattr(
            &mut buf,
            &[(b"mime_type", b"text/plain"), (b"tag", b"test")],
        );

        let mut cursor = Cursor::new(buf);
        let ndx = cache.receive_xattr(&mut cursor, false, 1).unwrap();

        assert_eq!(ndx, 0);
        assert_eq!(cache.len(), 1);

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.entries()[0].name(), expected_local_name(b"mime_type"));
        assert_eq!(list.entries()[0].datum(), b"text/plain");
        assert_eq!(list.entries()[1].name(), expected_local_name(b"tag"));
        assert_eq!(list.entries()[1].datum(), b"test");
    }

    #[test]
    fn receive_cache_hit() {
        let mut cache = XattrCache::new();

        // First, receive a literal set
        let mut buf = Vec::new();
        write_literal_xattr(&mut buf, &[(b"attr", b"value")]);
        let mut cursor = Cursor::new(buf);
        let first_ndx = cache.receive_xattr(&mut cursor, false, 1).unwrap();
        assert_eq!(first_ndx, 0);

        // Second, receive a cache hit referencing the first set
        let mut buf = Vec::new();
        write_cache_hit(&mut buf, 0);
        let mut cursor = Cursor::new(buf);
        let hit_ndx = cache.receive_xattr(&mut cursor, false, 1).unwrap();
        assert_eq!(hit_ndx, 0);

        // Cache should still have only one entry
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn receive_multiple_literal_sets() {
        let mut cache = XattrCache::new();

        let mut buf = Vec::new();
        write_literal_xattr(&mut buf, &[(b"a", b"val_a")]);
        write_literal_xattr(&mut buf, &[(b"b", b"val_b")]);

        let mut cursor = Cursor::new(buf);
        let ndx0 = cache.receive_xattr(&mut cursor, false, 1).unwrap();
        let ndx1 = cache.receive_xattr(&mut cursor, false, 1).unwrap();

        assert_eq!(ndx0, 0);
        assert_eq!(ndx1, 1);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn receive_empty_xattr_set() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        write_literal_xattr(&mut buf, &[]);

        let mut cursor = Cursor::new(buf);
        let ndx = cache.receive_xattr(&mut cursor, false, 1).unwrap();
        assert_eq!(ndx, 0);

        let list = cache.get(0).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn receive_abbreviated_xattr() {
        let mut cache = XattrCache::new();
        let large_value = vec![0xBB; 100]; // > MAX_FULL_DATUM

        let mut buf = Vec::new();
        write_literal_xattr(&mut buf, &[(b"large", &large_value)]);

        let mut cursor = Cursor::new(buf);
        let ndx = cache.receive_xattr(&mut cursor, false, 1).unwrap();
        assert_eq!(ndx, 0);

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list.entries()[0].is_abbreviated());
        assert_eq!(list.entries()[0].datum_len(), 100);
        assert_eq!(list.entries()[0].datum().len(), MAX_XATTR_DIGEST_LEN);
        assert_eq!(list.entries()[0].state(), XattrState::Abbrev);
    }

    #[test]
    fn receive_out_of_range_index_fails() {
        let mut cache = XattrCache::new();

        // Write an index that references beyond the cache
        let mut buf = Vec::new();
        write_varint(&mut buf, 5).unwrap(); // ndx=5 but cache is empty
        let mut cursor = Cursor::new(buf);

        let result = cache.receive_xattr(&mut cursor, false, 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn receive_negative_index_fails() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        write_varint(&mut buf, -1).unwrap();
        let mut cursor = Cursor::new(buf);

        let result = cache.receive_xattr(&mut cursor, false, 1);
        assert!(result.is_err());
    }

    #[test]
    fn receive_missing_nul_terminator_fails() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();

        // ndx = 0 (literal)
        write_varint(&mut buf, 0).unwrap();
        // count = 1
        write_varint(&mut buf, 1).unwrap();
        // name_len = 4 (no NUL at end)
        write_varint(&mut buf, 4).unwrap();
        // datum_len = 1
        write_varint(&mut buf, 1).unwrap();
        // name without NUL
        buf.extend_from_slice(b"test");
        // value
        buf.push(0x42);

        let mut cursor = Cursor::new(buf);
        let result = cache.receive_xattr(&mut cursor, false, 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("NUL"));
    }

    #[test]
    fn receive_xattr_with_empty_value() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        write_literal_xattr(&mut buf, &[(b"empty", b"")]);

        let mut cursor = Cursor::new(buf);
        let ndx = cache.receive_xattr(&mut cursor, false, 1).unwrap();
        assert_eq!(ndx, 0);

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list.entries()[0].name(), expected_local_name(b"empty"));
        assert!(list.entries()[0].datum().is_empty());
    }

    #[test]
    fn receive_xattr_entry_num_is_1_based() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        write_literal_xattr(
            &mut buf,
            &[(b"first", b"a"), (b"second", b"b"), (b"third", b"c")],
        );

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        // Entry nums are preserved from wire order even after sorting
        let nums: Vec<u32> = list.entries().iter().map(|e| e.num()).collect();
        assert!(nums.contains(&1));
        assert!(nums.contains(&2));
        assert!(nums.contains(&3));
    }

    #[test]
    fn get_mut_allows_modification() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        write_literal_xattr(&mut buf, &[(b"test", b"original")]);

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get_mut(0).unwrap();
        list.entries_mut()[0].set_full_value(b"modified".to_vec());

        assert_eq!(cache.get(0).unwrap().entries()[0].datum(), b"modified");
    }

    #[test]
    fn store_returns_sequential_indices() {
        let mut cache = XattrCache::new();
        let ndx0 = cache.store(XattrList::new());
        let ndx1 = cache.store(XattrList::new());
        let ndx2 = cache.store(XattrList::new());

        assert_eq!(ndx0, 0);
        assert_eq!(ndx1, 1);
        assert_eq!(ndx2, 2);
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn cache_hit_after_multiple_stores() {
        let mut cache = XattrCache::new();

        // Store three literal sets
        let mut buf = Vec::new();
        write_literal_xattr(&mut buf, &[(b"a", b"1")]);
        write_literal_xattr(&mut buf, &[(b"b", b"2")]);
        write_literal_xattr(&mut buf, &[(b"c", b"3")]);

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();
        cache.receive_xattr(&mut cursor, false, 1).unwrap();
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        // Now reference the second set (index 1)
        let mut buf = Vec::new();
        write_cache_hit(&mut buf, 1);
        let mut cursor = Cursor::new(buf);
        let hit = cache.receive_xattr(&mut cursor, false, 1).unwrap();
        assert_eq!(hit, 1);

        // Verify the referenced set
        let list = cache.get(1).unwrap();
        assert_eq!(list.entries()[0].name(), expected_local_name(b"b"));
    }

    #[test]
    fn receive_mixed_small_and_large_values() {
        let mut cache = XattrCache::new();
        let large_value = vec![0xCC; 64];

        let mut buf = Vec::new();
        write_literal_xattr(
            &mut buf,
            &[
                (b"also_small", b"also tiny"),
                (b"large", &large_value),
                (b"small", b"tiny"),
            ],
        );

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 3);
        // After sorting, order depends on local names
        let has_abbreviated = list.entries().iter().any(|e| e.is_abbreviated());
        let has_full = list.entries().iter().any(|e| !e.is_abbreviated());
        assert!(has_abbreviated);
        assert!(has_full);
    }

    #[test]
    fn receive_name_translation_applied() {
        // Verify wire names are translated to local names
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        write_literal_xattr(&mut buf, &[(b"my_attr", b"my_value")]);

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list.entries()[0].name(), expected_local_name(b"my_attr"));
        assert_eq!(list.entries()[0].datum(), b"my_value");
    }

    #[test]
    fn receive_rsync_internal_filtered_at_level_1() {
        // rsync.%stat should be filtered when preserve_xattrs == 1
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();

        let rsync_prefix = RSYNC_PREFIX;
        let internal_name = format!("{rsync_prefix}%stat");
        write_literal_xattr(
            &mut buf,
            &[
                (internal_name.as_bytes(), b"internal"),
                (b"normal_attr", b"kept"),
            ],
        );

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        // The internal attr should be filtered out
        assert_eq!(list.len(), 1);
        assert_eq!(
            list.entries()[0].name(),
            expected_local_name(b"normal_attr")
        );
    }

    #[test]
    fn receive_rsync_internal_kept_at_level_2() {
        // rsync.%stat should be kept when preserve_xattrs == 2 (double -X)
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();

        let rsync_prefix = RSYNC_PREFIX;
        let internal_name = format!("{rsync_prefix}%stat");
        write_literal_xattr(
            &mut buf,
            &[
                (internal_name.as_bytes(), b"internal"),
                (b"normal_attr", b"kept"),
            ],
        );

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 2).unwrap();

        let list = cache.get(0).unwrap();
        // Both entries should be kept
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn receive_entries_sorted_by_name() {
        // Names should be sorted after translation
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        write_literal_xattr(
            &mut buf,
            &[(b"zebra", b"z"), (b"alpha", b"a"), (b"middle", b"m")],
        );

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 3);
        // Entries should be sorted alphabetically by local name
        let names: Vec<&[u8]> = list.entries().iter().map(|e| e.name()).collect();
        let mut sorted_names = names.clone();
        sorted_names.sort();
        assert_eq!(names, sorted_names);
    }

    #[test]
    fn is_rsync_internal_attr_detection() {
        let rpre = RSYNC_PREFIX;
        let stat_name = format!("{rpre}%stat").into_bytes();
        let aacl_name = format!("{rpre}%aacl").into_bytes();
        let normal_name = format!("{rpre}normal").into_bytes();

        assert!(is_rsync_internal_attr(&stat_name));
        assert!(is_rsync_internal_attr(&aacl_name));
        assert!(!is_rsync_internal_attr(&normal_name));
        assert!(!is_rsync_internal_attr(b"regular_attr"));
    }
}
