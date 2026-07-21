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

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::io::{self, Read};

use crate::varint::read_varint;
use crate::xattr::prefix::wire_to_local;
use crate::xattr::{MAX_FULL_DATUM, MAX_XATTR_DIGEST_LEN, RSYNC_PREFIX, XattrEntry, XattrList};

/// Cache of xattr sets, indexed for deduplication.
///
/// Mirrors upstream's `rsync_xal_l` item list. Each file entry references
/// an xattr set by index into this cache, avoiding duplicate storage of
/// identical xattr sets across multiple files.
///
/// A hash index (`by_hash`) mirrors upstream's `rsync_xal_h` hash table
/// (`xattrs.c:xattr_lookup_hash`), giving [`find`](Self::find) amortized
/// O(1) lookup instead of a linear scan over every stored set. The index is
/// maintained only by [`store`](Self::store) (the sole insertion path), so
/// it stays consistent with `lists`. [`get_mut`](Self::get_mut) can mutate a
/// stored set's datum after the fact, which would stale the hash for that
/// slot; that path is receiver-only and never feeds [`find`](Self::find)
/// (a sender-only operation), so lookup correctness is unaffected.
#[derive(Debug, Default, Clone)]
pub struct XattrCache {
    /// Stored xattr lists, indexed by position.
    lists: Vec<XattrList>,
    /// Maps an xattr set's content hash to the slot indices sharing it.
    ///
    /// Collisions (and distinct sets that hash alike) are disambiguated by a
    /// full element-wise comparison in [`find`](Self::find), matching
    /// upstream's collision walk in `xattrs.c:find_matching_xattr()`.
    by_hash: HashMap<u64, Vec<u32>>,
}

impl XattrCache {
    /// Creates an empty xattr cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            lists: Vec::new(),
            by_hash: HashMap::new(),
        }
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
    ///
    /// Also records the set's content hash in the lookup index so subsequent
    /// [`find`](Self::find) calls resolve in amortized O(1).
    ///
    /// # Upstream Reference
    ///
    /// See `xattrs.c:rsync_xal_store()` - appends to `rsync_xal_l` and inserts
    /// the computed key into `rsync_xal_h`.
    #[must_use]
    pub fn store(&mut self, list: XattrList) -> u32 {
        let index = self.lists.len() as u32;
        let key = Self::hash_list(&list);
        self.by_hash.entry(key).or_default().push(index);
        self.lists.push(list);
        index
    }

    /// Finds a stored xattr set identical to `list`, returning its index.
    ///
    /// Uses the content-hash index to select candidate slots, then confirms a
    /// match with a full element-wise comparison (entry count plus per-entry
    /// name, datum length, and datum equality). Returns the index of the first
    /// stored set that matches, or `None` if none does.
    ///
    /// This is the sender-side deduplication lookup: a hit lets the writer emit
    /// a compact cache reference instead of re-transmitting the literal set.
    /// The returned index is the 1-based-minus-one slot number used for the
    /// wire abbreviation, so the assignment order (and thus the wire output) is
    /// identical to a linear scan.
    ///
    /// # Upstream Reference
    ///
    /// See `xattrs.c:find_matching_xattr()` - hashed lookup in `rsync_xal_h`
    /// followed by an element-wise walk of the colliding candidates.
    #[must_use]
    pub fn find(&self, list: &XattrList) -> Option<u32> {
        let key = Self::hash_list(list);
        for &index in self.by_hash.get(&key)? {
            if let Some(cached) = self.lists.get(index as usize) {
                if Self::lists_equal(cached, list) {
                    return Some(index);
                }
            }
        }
        None
    }

    /// Element-wise equality of two xattr sets.
    ///
    /// Two sets match when they have the same entry count and, positionally,
    /// each entry shares the same name, datum length, and datum bytes (the
    /// datum being the checksum for abbreviated entries). Mirrors the compare
    /// loop in `xattrs.c:find_matching_xattr()`.
    fn lists_equal(a: &XattrList, b: &XattrList) -> bool {
        a.len() == b.len()
            && a.iter().zip(b.iter()).all(|(x, y)| {
                x.name() == y.name() && x.datum_len() == y.datum_len() && x.datum() == y.datum()
            })
    }

    /// Computes a content hash for an xattr set consistent with
    /// [`lists_equal`](Self::lists_equal).
    ///
    /// Hashes the entry count followed by each entry's name, datum length, and
    /// datum bytes in list order. Equal sets always hash identically; unequal
    /// sets that collide are separated by the full comparison in
    /// [`find`](Self::find).
    ///
    /// # Upstream Reference
    ///
    /// See `xattrs.c:xattr_lookup_hash()` - folds the count and each entry's
    /// name and datum into the lookup key.
    fn hash_list(list: &XattrList) -> u64 {
        let mut hasher = DefaultHasher::new();
        hasher.write_usize(list.len());
        for entry in list.iter() {
            hasher.write(entry.name());
            hasher.write_usize(entry.datum_len());
            hasher.write(entry.datum());
        }
        hasher.finish()
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
        // upstream: xattrs.c:863 - need_sort is set whenever name
        // translation mutates a name. Linux keeps user.* verbatim and drops
        // (non-root) or keeps verbatim (root) a non-user name, so a name is
        // never rewritten in place; non-Linux receivers always strip the
        // user. prefix, so wire ordering can diverge from local ordering
        // after translation.
        let mut need_sort = false;

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
    /// Names are written verbatim. Upstream rsync transmits names
    /// byte-for-byte from `listxattr(2)` (so a Linux peer keeps the
    /// `user.` prefix on the wire); callers should supply names exactly
    /// as they would appear in the protocol stream.
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

    /// Returns the expected local name after `wire_to_local` translation
    /// for a `user.*` wire name. On Linux the prefix is preserved
    /// verbatim, on non-Linux the prefix is stripped.
    fn expected_local_for_user_wire(base: &[u8]) -> Vec<u8> {
        let mut wire = b"user.".to_vec();
        wire.extend_from_slice(base);
        #[cfg(target_os = "linux")]
        {
            wire
        }
        #[cfg(not(target_os = "linux"))]
        {
            base.to_vec()
        }
    }

    /// Returns the wire-format name a Linux peer would emit for a
    /// `user.<base>` xattr. Used by the helper tests to construct
    /// realistic wire payloads.
    fn user_wire_name(base: &[u8]) -> Vec<u8> {
        let mut wire = b"user.".to_vec();
        wire.extend_from_slice(base);
        wire
    }

    #[test]
    fn receive_literal_xattr_set() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        let mime = user_wire_name(b"mime_type");
        let tag = user_wire_name(b"tag");
        write_literal_xattr(&mut buf, &[(&mime, b"text/plain"), (&tag, b"test")]);

        let mut cursor = Cursor::new(buf);
        let ndx = cache.receive_xattr(&mut cursor, false, 1).unwrap();

        assert_eq!(ndx, 0);
        assert_eq!(cache.len(), 1);

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.entries()[0].name(),
            expected_local_for_user_wire(b"mime_type"),
        );
        assert_eq!(list.entries()[0].datum(), b"text/plain");
        assert_eq!(
            list.entries()[1].name(),
            expected_local_for_user_wire(b"tag"),
        );
        assert_eq!(list.entries()[1].datum(), b"test");
    }

    #[test]
    fn receive_cache_hit() {
        let mut cache = XattrCache::new();

        // First, receive a literal set
        let mut buf = Vec::new();
        let attr = user_wire_name(b"attr");
        write_literal_xattr(&mut buf, &[(&attr, b"value")]);
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
        let a = user_wire_name(b"a");
        let b = user_wire_name(b"b");
        write_literal_xattr(&mut buf, &[(&a, b"val_a")]);
        write_literal_xattr(&mut buf, &[(&b, b"val_b")]);

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
        let large = user_wire_name(b"large");
        write_literal_xattr(&mut buf, &[(&large, &large_value)]);

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
        let empty = user_wire_name(b"empty");
        write_literal_xattr(&mut buf, &[(&empty, b"")]);

        let mut cursor = Cursor::new(buf);
        let ndx = cache.receive_xattr(&mut cursor, false, 1).unwrap();
        assert_eq!(ndx, 0);

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list.entries()[0].name(),
            expected_local_for_user_wire(b"empty"),
        );
        assert!(list.entries()[0].datum().is_empty());
    }

    #[test]
    fn receive_xattr_entry_num_is_1_based() {
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        let first = user_wire_name(b"first");
        let second = user_wire_name(b"second");
        let third = user_wire_name(b"third");
        write_literal_xattr(&mut buf, &[(&first, b"a"), (&second, b"b"), (&third, b"c")]);

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
        let test = user_wire_name(b"test");
        write_literal_xattr(&mut buf, &[(&test, b"original")]);

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
        let a = user_wire_name(b"a");
        let b = user_wire_name(b"b");
        let c = user_wire_name(b"c");
        write_literal_xattr(&mut buf, &[(&a, b"1")]);
        write_literal_xattr(&mut buf, &[(&b, b"2")]);
        write_literal_xattr(&mut buf, &[(&c, b"3")]);

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
        assert_eq!(list.entries()[0].name(), expected_local_for_user_wire(b"b"),);
    }

    #[test]
    fn receive_mixed_small_and_large_values() {
        let mut cache = XattrCache::new();
        let large_value = vec![0xCC; 64];

        let mut buf = Vec::new();
        let also_small = user_wire_name(b"also_small");
        let large = user_wire_name(b"large");
        let small = user_wire_name(b"small");
        write_literal_xattr(
            &mut buf,
            &[
                (&also_small, b"also tiny"),
                (&large, &large_value),
                (&small, b"tiny"),
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
        // Verify wire names are translated to local names. On Linux the
        // user.* wire name is kept verbatim; on non-Linux the user.
        // prefix is stripped.
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        let my_attr = user_wire_name(b"my_attr");
        write_literal_xattr(&mut buf, &[(&my_attr, b"my_value")]);

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list.entries()[0].name(),
            expected_local_for_user_wire(b"my_attr"),
        );
        assert_eq!(list.entries()[0].datum(), b"my_value");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn receive_rsync_internal_filtered_at_level_1() {
        // The internal-attribute slot (user.rsync.%stat) must be
        // filtered when preserve_xattrs == 1. Linux-only because
        // non-Linux receivers drop every non-user-prefixed wire name
        // for non-root callers (upstream xattrs.c:844), so the slot
        // would never survive translation to reach the level check.
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();

        let internal_name = format!("{RSYNC_PREFIX}%stat");
        let normal_attr = user_wire_name(b"normal_attr");
        write_literal_xattr(
            &mut buf,
            &[
                (internal_name.as_bytes(), b"internal"),
                (&normal_attr, b"kept"),
            ],
        );

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        // The internal attr should be filtered out
        assert_eq!(list.len(), 1);
        assert_eq!(
            list.entries()[0].name(),
            expected_local_for_user_wire(b"normal_attr"),
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn receive_rsync_internal_kept_at_level_2() {
        // The internal-attribute slot is kept when preserve_xattrs == 2
        // (double `-X`), matching upstream xattrs.c:849. Linux-only for
        // the same reason as the level-1 test above.
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();

        let internal_name = format!("{RSYNC_PREFIX}%stat");
        let normal_attr = user_wire_name(b"normal_attr");
        write_literal_xattr(
            &mut buf,
            &[
                (internal_name.as_bytes(), b"internal"),
                (&normal_attr, b"kept"),
            ],
        );

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 2).unwrap();

        let list = cache.get(0).unwrap();
        // Both entries should be kept
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn receive_entries_preserve_wire_order() {
        // upstream: xattrs.c:863 - the receiver only re-sorts when name
        // translation (iconv-style) changes the order. With names preserved
        // verbatim from the wire, the sender-sorted order is preserved as-is.
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();
        let alpha = user_wire_name(b"alpha");
        let middle = user_wire_name(b"middle");
        let zebra = user_wire_name(b"zebra");
        // Wire arrives already sorted (upstream sender invariant).
        write_literal_xattr(&mut buf, &[(&alpha, b"a"), (&middle, b"m"), (&zebra, b"z")]);

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        assert_eq!(list.len(), 3);
        let names: Vec<Vec<u8>> = list.entries().iter().map(|e| e.name().to_vec()).collect();
        assert_eq!(
            names,
            vec![
                expected_local_for_user_wire(b"alpha"),
                expected_local_for_user_wire(b"middle"),
                expected_local_for_user_wire(b"zebra"),
            ],
        );
    }

    #[test]
    fn receive_entries_preserve_order_after_skips() {
        // upstream: xattrs.c qsort_cmp - 3.4.2 fix uses temp_xattr.count, not
        // the wire count. Verifies that entries skipped via `continue` (here,
        // the rsync.%stat internal attr at preserve_xattrs == 1) do not leave
        // a leak or misalignment in the cached XattrList. Input is in the
        // sender-sorted order (with the internal attr inlined between sorted
        // user entries); receiver filters the internal entry without
        // disturbing the surrounding user-entry order.
        let mut cache = XattrCache::new();
        let mut buf = Vec::new();

        let internal_name = format!("{RSYNC_PREFIX}%stat");
        let alpha = user_wire_name(b"alpha");
        let middle = user_wire_name(b"middle");
        let zeta = user_wire_name(b"zeta");
        write_literal_xattr(
            &mut buf,
            &[
                (&alpha, b"a"),
                (internal_name.as_bytes(), b"internal"),
                (&middle, b"m"),
                (&zeta, b"z"),
            ],
        );

        let mut cursor = Cursor::new(buf);
        cache.receive_xattr(&mut cursor, false, 1).unwrap();

        let list = cache.get(0).unwrap();
        // Internal entry filtered; remaining three keep wire-arrival order.
        assert_eq!(list.len(), 3);
        let names: Vec<Vec<u8>> = list.entries().iter().map(|e| e.name().to_vec()).collect();
        assert_eq!(
            names,
            vec![
                expected_local_for_user_wire(b"alpha"),
                expected_local_for_user_wire(b"middle"),
                expected_local_for_user_wire(b"zeta"),
            ],
        );
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

    /// Builds an [`XattrList`] from `(name, value)` pairs, as the sender would
    /// present it to [`XattrCache::store`] / [`XattrCache::find`].
    fn list_from(pairs: &[(&[u8], &[u8])]) -> XattrList {
        let mut list = XattrList::new();
        for (num, &(name, value)) in pairs.iter().enumerate() {
            let mut entry = XattrEntry::new(name.to_vec(), value.to_vec());
            entry.set_num((num + 1) as u32);
            list.push(entry);
        }
        list
    }

    /// Reference linear scan matching the pre-index sender lookup: entry count
    /// plus positional name/datum_len/datum equality. Used to prove the hashed
    /// [`XattrCache::find`] assigns identical dedup indices (and thus identical
    /// wire output).
    fn linear_find(lists: &[XattrList], list: &XattrList) -> Option<u32> {
        for (i, cached) in lists.iter().enumerate() {
            if cached.len() != list.len() {
                continue;
            }
            let all_match = cached.iter().zip(list.iter()).all(|(a, b)| {
                a.name() == b.name() && a.datum_len() == b.datum_len() && a.datum() == b.datum()
            });
            if all_match {
                return Some(i as u32);
            }
        }
        None
    }

    #[test]
    fn find_resolves_stored_set_by_content() {
        let mut cache = XattrCache::new();
        let ndx0 = cache.store(list_from(&[(b"user.a", b"1")]));
        let ndx1 = cache.store(list_from(&[(b"user.b", b"2"), (b"user.c", b"3")]));
        assert_eq!(ndx0, 0);
        assert_eq!(ndx1, 1);

        // A structurally identical set (fresh entries) resolves to the stored slot.
        assert_eq!(cache.find(&list_from(&[(b"user.a", b"1")])), Some(0));
        assert_eq!(
            cache.find(&list_from(&[(b"user.b", b"2"), (b"user.c", b"3")])),
            Some(1),
        );
        // Absent sets miss: unknown name, differing value, and differing order.
        assert_eq!(cache.find(&list_from(&[(b"user.z", b"9")])), None);
        assert_eq!(cache.find(&list_from(&[(b"user.a", b"2")])), None);
        assert_eq!(
            cache.find(&list_from(&[(b"user.c", b"3"), (b"user.b", b"2")])),
            None,
        );
    }

    #[test]
    fn find_dedup_indices_match_linear_scan() {
        // A stream of files with repeated + distinct xattr sets. The hashed
        // find must assign exactly the indices a linear scan would, so the
        // wire NUM abbreviation (and thus the byte output) is unchanged.
        let files = [
            list_from(&[(b"user.a", b"1")]),
            list_from(&[(b"user.b", b"2")]),
            list_from(&[(b"user.a", b"1")]), // dup of file 0
            list_from(&[(b"user.c", b"3"), (b"user.d", b"4")]),
            list_from(&[(b"user.b", b"2")]), // dup of file 1
            list_from(&[(b"user.c", b"3"), (b"user.d", b"4")]), // dup of file 3
            XattrList::new(),                // empty set, distinct
            XattrList::new(),                // dup of the empty set
        ];

        let mut cache = XattrCache::new();
        let mut reference: Vec<XattrList> = Vec::new();

        for file in &files {
            let hashed = cache.find(file);
            let linear = linear_find(&reference, file);
            assert_eq!(hashed, linear, "hashed find diverged from linear scan");

            if hashed.is_none() {
                let ndx = cache.store(file.clone());
                assert_eq!(ndx as usize, reference.len(), "store index diverged");
                reference.push(file.clone());
            }
        }

        // Deduplication collapsed the four unique sets to four slots.
        assert_eq!(cache.len(), 4);
    }

    #[test]
    fn find_is_hash_based_at_scale() {
        // Populate the cache with many distinct sets, then confirm each is
        // resolved to its own slot. A linear scan would be O(n) per lookup;
        // this exercises the hash index at a size where correctness (not just
        // adequacy) matters.
        const N: u32 = 5000;
        let mut cache = XattrCache::new();
        for i in 0..N {
            let name = format!("user.attr{i}").into_bytes();
            let value = format!("value{i}").into_bytes();
            let ndx = cache.store(list_from(&[(&name, &value)]));
            assert_eq!(ndx, i);
        }

        for i in 0..N {
            let name = format!("user.attr{i}").into_bytes();
            let value = format!("value{i}").into_bytes();
            assert_eq!(cache.find(&list_from(&[(&name, &value)])), Some(i));
        }

        // A set that was never stored misses.
        assert_eq!(cache.find(&list_from(&[(b"user.absent", b"nope")])), None,);
    }

    #[test]
    fn find_confirms_on_hash_collision_candidates() {
        // Store two sets that differ only in datum; regardless of whether their
        // hashes collide, find must return the exact matching slot and never a
        // near-miss sharing the same bucket.
        let mut cache = XattrCache::new();
        let ndx0 = cache.store(list_from(&[(b"user.k", b"aaaa")]));
        let ndx1 = cache.store(list_from(&[(b"user.k", b"bbbb")]));
        assert_eq!((ndx0, ndx1), (0, 1));

        assert_eq!(cache.find(&list_from(&[(b"user.k", b"aaaa")])), Some(0));
        assert_eq!(cache.find(&list_from(&[(b"user.k", b"bbbb")])), Some(1));
        assert_eq!(cache.find(&list_from(&[(b"user.k", b"cccc")])), None);
    }
}
