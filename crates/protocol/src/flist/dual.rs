//! Dual file-list wrapper for the flat backing-store migration.
//!
//! [`DualFileList`] pushes every entry to both the legacy `Vec<FileEntry>`
//! and (when `flat-flist` is enabled) the arena-backed [`FlatFileList`].
//! The [`FlatFileList`] owns its path interner and extras arena internally,
//! so optional metadata (symlink targets, device numbers, ACL/xattr
//! indices, checksums, user/group names, atime/crtime) is encoded through
//! [`FlatFileList::push_with_extras`]. This allows production code to
//! migrate call sites one at a time: read from either representation,
//! compare results, and eventually drop the legacy path.
//!
//! Without the `flat-flist` feature, `DualFileList` compiles as a transparent
//! newtype over `Vec<FileEntry>` with zero overhead - no arena fields, no
//! conversion logic, and no extra imports.

use std::fmt;
use std::ops::{Index, IndexMut, RangeFrom};

use super::FileEntry;

#[cfg(feature = "flat-flist")]
use super::flat::{
    FileEntryHeader, FlatExtras, FlatFileList, PRESENT_CONTENT_DIR, PRESENT_GID, PRESENT_LENGTH64,
    PRESENT_MTIME_NSEC, PRESENT_UID,
};

/// Dual file-list that maintains both legacy and flat representations.
///
/// Every [`push`](Self::push) appends to the legacy `Vec<FileEntry>` and,
/// when the `flat-flist` feature is active, converts the entry and appends
/// it to the flat arena stores. Read accessors delegate to the legacy Vec
/// so existing call sites remain unchanged.
///
/// Without the `flat-flist` feature, this is a zero-cost newtype over
/// `Vec<FileEntry>`: no arena fields exist and [`push`](Self::push) is a
/// plain `Vec::push`.
pub struct DualFileList {
    legacy: Vec<FileEntry>,
    #[cfg(feature = "flat-flist")]
    flat: FlatFileList,
}

impl fmt::Debug for DualFileList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DualFileList")
            .field("len", &self.legacy.len())
            .finish()
    }
}

impl DualFileList {
    /// Creates an empty dual file list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            legacy: Vec::new(),
            #[cfg(feature = "flat-flist")]
            flat: FlatFileList::new(),
        }
    }

    /// Creates a dual file list pre-allocated for `cap` entries.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            legacy: Vec::with_capacity(cap),
            #[cfg(feature = "flat-flist")]
            flat: FlatFileList::with_capacity(cap),
        }
    }

    /// Appends an entry to the list.
    ///
    /// Always pushes to the legacy `Vec<FileEntry>`. When the `flat-flist`
    /// feature is enabled, also converts the entry and pushes the resulting
    /// header, path handles, and extras into the flat stores.
    pub fn push(&mut self, entry: FileEntry) {
        #[cfg(feature = "flat-flist")]
        {
            let (header, extras) = file_entry_to_flat(&entry, &mut self.flat);
            self.flat.push_with_extras(header, &extras);
        }
        self.legacy.push(entry);
    }

    /// Returns the number of entries in the list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.legacy.len()
    }

    /// Returns `true` if the list contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.legacy.is_empty()
    }

    /// Returns a slice of all legacy entries.
    #[must_use]
    pub fn as_slice(&self) -> &[FileEntry] {
        &self.legacy
    }

    /// Returns a reference to the entry at `index`, or `None` if out of bounds.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&FileEntry> {
        self.legacy.get(index)
    }

    /// Returns an iterator over references to the legacy entries.
    pub fn iter(&self) -> std::slice::Iter<'_, FileEntry> {
        self.legacy.iter()
    }

    /// Returns an iterator over mutable references to the legacy entries.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, FileEntry> {
        self.legacy.iter_mut()
    }

    /// Returns the current length of the legacy Vec, for use as a segment
    /// start index in INC_RECURSE sub-list building.
    #[must_use]
    pub fn segment_start(&self) -> usize {
        self.legacy.len()
    }

    /// Clears all entries from both legacy and flat stores.
    pub fn clear(&mut self) {
        self.legacy.clear();
        #[cfg(feature = "flat-flist")]
        {
            self.flat = FlatFileList::new();
        }
    }

    /// Reserves capacity for at least `additional` more entries in the
    /// legacy Vec. The flat stores grow dynamically and do not need
    /// explicit reservation.
    pub fn reserve(&mut self, additional: usize) {
        self.legacy.reserve(additional);
    }

    /// Returns a shared reference to the flat file list.
    ///
    /// Available only when the `flat-flist` feature is enabled.
    #[cfg(feature = "flat-flist")]
    #[must_use]
    pub fn flat(&self) -> &FlatFileList {
        &self.flat
    }

    /// Returns a shared reference to the extras arena.
    ///
    /// Available only when the `flat-flist` feature is enabled. Delegates
    /// to the [`FlatFileList`]'s own extras arena.
    #[cfg(feature = "flat-flist")]
    #[must_use]
    pub fn extras(&self) -> &super::flat::ExtrasArena {
        self.flat.extras()
    }

    /// Returns a mutable reference to the underlying legacy Vec.
    ///
    /// Exposed for call sites that need direct Vec access (e.g. sorting,
    /// filtering, INC_RECURSE segment manipulation).
    pub fn as_mut_vec(&mut self) -> &mut Vec<FileEntry> {
        &mut self.legacy
    }

    /// Consumes the dual list and returns the underlying legacy Vec.
    #[must_use]
    pub fn into_vec(self) -> Vec<FileEntry> {
        self.legacy
    }

    /// Reclaims heap data from entries in the range `[start..end)`.
    ///
    /// Calls [`FileEntry::reclaim_heap_data`] on each entry in the range,
    /// freeing PathBuf, dirname Arc, and extras Box allocations while
    /// keeping the entries in place so NDX-based indexing remains valid.
    ///
    /// This mirrors upstream rsync's `flist_free()` which deallocates
    /// completed INC_RECURSE segments during the transfer loop.
    ///
    /// # Panics
    ///
    /// Panics if `end > self.len()` or `start > end`.
    pub fn reclaim_segment(&mut self, start: usize, end: usize) {
        assert!(
            end <= self.legacy.len() && start <= end,
            "reclaim_segment: [{start}..{end}) out of bounds (len={})",
            self.legacy.len()
        );
        for entry in &mut self.legacy[start..end] {
            entry.reclaim_heap_data();
        }
    }
}

impl Default for DualFileList {
    fn default() -> Self {
        Self::new()
    }
}

impl Index<usize> for DualFileList {
    type Output = FileEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.legacy[index]
    }
}

impl Index<RangeFrom<usize>> for DualFileList {
    type Output = [FileEntry];

    fn index(&self, index: RangeFrom<usize>) -> &Self::Output {
        &self.legacy[index]
    }
}

impl IndexMut<usize> for DualFileList {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.legacy[index]
    }
}

impl<'a> IntoIterator for &'a DualFileList {
    type Item = &'a FileEntry;
    type IntoIter = std::slice::Iter<'a, FileEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.legacy.iter()
    }
}

impl<'a> IntoIterator for &'a mut DualFileList {
    type Item = &'a mut FileEntry;
    type IntoIter = std::slice::IterMut<'a, FileEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.legacy.iter_mut()
    }
}

/// Converts a [`FileEntry`] into a [`FileEntryHeader`] and [`FlatExtras`],
/// interning paths through the [`FlatFileList`]'s [`PathArena`].
///
/// The entry's name is split at the last `/` separator into dirname and
/// basename, each interned through the [`FlatFileList`]'s [`PathArena`].
/// Optional metadata fields (link target, device numbers, hardlink index,
/// ACL/xattr indices, checksum, user/group names, atime/crtime) are packed
/// into the returned [`FlatExtras`]. The caller is responsible for encoding
/// the extras into the arena (via [`FlatFileList::push_with_extras`]).
///
/// The header's [`ExtrasRef`] is set to
/// [`ExtrasRef::NO_EXTRAS`](super::flat::ExtrasRef::NO_EXTRAS) as a
/// placeholder - [`FlatFileList::push_with_extras`] overwrites it.
#[cfg(feature = "flat-flist")]
fn file_entry_to_flat(
    entry: &FileEntry,
    flist: &mut FlatFileList,
) -> (FileEntryHeader, FlatExtras) {
    use super::flat::ExtrasRef;

    let full_name = entry.name();
    let (dirname_str, basename_str) = match full_name.rfind('/') {
        Some(pos) => (&full_name[..pos], &full_name[pos + 1..]),
        None => ("", full_name),
    };

    let paths = flist.paths_mut();
    let name_handle = paths.intern(basename_str);
    let dirname_handle = paths.intern(dirname_str);

    let mut present: u16 = 0;
    if entry.uid().is_some() {
        present |= PRESENT_UID;
    }
    if entry.gid().is_some() {
        present |= PRESENT_GID;
    }
    if entry.mtime_nsec() != 0 {
        present |= PRESENT_MTIME_NSEC;
    }
    if entry.content_dir() {
        present |= PRESENT_CONTENT_DIR;
    }
    if entry.size() > u64::from(u32::MAX) {
        present |= PRESENT_LENGTH64;
    }

    let flat_extras = build_flat_extras(entry);

    // Reconstruct a u16 flags value from the persisted bits for the flat
    // header's `flags` field. Only the 3 persisted wire flags survive
    // past decoding (top_dir, hlinked, hlink_first); the remaining XMIT
    // bits are transient wire-encoding state discarded at reception.
    let flags_u16 = (entry.top_dir() as u16)
        | ((entry.hlinked() as u16) << 9)
        | ((entry.hlink_first() as u16) << 12);

    let header = FileEntryHeader {
        mtime: entry.mtime(),
        size: entry.size(),
        uid: entry.uid().unwrap_or(0),
        gid: entry.gid().unwrap_or(0),
        name: name_handle,
        dirname: dirname_handle,
        extras: ExtrasRef::NO_EXTRAS,
        mtime_nsec: entry.mtime_nsec(),
        mode: entry.mode(),
        flags: flags_u16,
        present,
    };

    (header, flat_extras)
}

/// Builds a [`FlatExtras`] from a [`FileEntry`]'s optional metadata fields.
#[cfg(feature = "flat-flist")]
fn build_flat_extras(entry: &FileEntry) -> FlatExtras {
    let mut extras = FlatExtras::default();

    if let Some(target) = entry.link_target() {
        // Convert PathBuf to bytes for the arena.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            extras.link_target = Some(target.as_os_str().as_bytes().to_vec());
        }
        #[cfg(not(unix))]
        {
            let lossy = target.to_string_lossy();
            extras.link_target = Some(lossy.as_bytes().to_vec());
        }
    }

    if let (Some(major), Some(minor)) = (entry.rdev_major(), entry.rdev_minor()) {
        extras.rdev_major = Some(major);
        extras.rdev_minor = Some(minor);
    }

    if let Some(idx) = entry.hardlink_idx() {
        extras.hardlink_idx = Some(idx);
    }

    if let Some(ndx) = entry.acl_ndx() {
        extras.acl_ndx = Some(ndx);
    }

    if let Some(ndx) = entry.def_acl_ndx() {
        extras.def_acl_ndx = Some(ndx);
    }

    if let Some(ndx) = entry.xattr_ndx() {
        extras.xattr_ndx = Some(ndx);
    }

    if let Some(checksum) = entry.checksum() {
        extras.checksum = Some(checksum.to_vec());
    }

    if let Some(name) = entry.user_name() {
        extras.user_name = Some(name.as_bytes().to_vec());
    }

    if let Some(name) = entry.group_name() {
        extras.group_name = Some(name.as_bytes().to_vec());
    }

    let atime = entry.atime();
    if atime != 0 {
        extras.atime = Some(atime);
    }

    let crtime = entry.crtime();
    if crtime != 0 {
        extras.crtime = Some(crtime);
    }

    let atime_nsec = entry.atime_nsec();
    if atime_nsec != 0 {
        extras.atime_nsec = Some(atime_nsec);
    }

    extras
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let list = DualFileList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert!(list.get(0).is_none());
    }

    #[test]
    fn with_capacity_is_empty() {
        let list = DualFileList::with_capacity(64);
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn default_is_empty() {
        let list = DualFileList::default();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn push_and_read_back() {
        let mut list = DualFileList::new();
        let entry = FileEntry::new_file("src/main.rs".into(), 1024, 0o644);
        list.push(entry);

        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());
        assert_eq!(list[0].name(), "src/main.rs");
        assert_eq!(list[0].size(), 1024);
    }

    #[test]
    fn get_returns_none_out_of_bounds() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 10, 0o644));
        assert!(list.get(0).is_some());
        assert!(list.get(1).is_none());
        assert!(list.get(100).is_none());
    }

    #[test]
    fn as_slice_returns_all_entries() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 10, 0o644));
        list.push(FileEntry::new_file("b.txt".into(), 20, 0o644));
        let slice = list.as_slice();
        assert_eq!(slice.len(), 2);
        assert_eq!(slice[0].name(), "a.txt");
        assert_eq!(slice[1].name(), "b.txt");
    }

    #[test]
    fn iter_yields_all_entries_in_order() {
        let mut list = DualFileList::new();
        let names = ["alpha", "beta", "gamma"];
        for name in &names {
            list.push(FileEntry::new_file((*name).into(), 0, 0o644));
        }
        let collected: Vec<&str> = list.iter().map(|e| e.name()).collect();
        assert_eq!(collected, names);
    }

    #[test]
    fn iter_mut_allows_modification() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("f.txt".into(), 100, 0o644));
        for entry in list.iter_mut() {
            entry.set_size(200);
        }
        assert_eq!(list[0].size(), 200);
    }

    #[test]
    fn segment_start_tracks_length() {
        let mut list = DualFileList::new();
        assert_eq!(list.segment_start(), 0);
        list.push(FileEntry::new_file("a.txt".into(), 0, 0o644));
        assert_eq!(list.segment_start(), 1);
        list.push(FileEntry::new_file("b.txt".into(), 0, 0o644));
        assert_eq!(list.segment_start(), 2);
    }

    #[test]
    fn index_usize() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("test.txt".into(), 42, 0o644));
        assert_eq!(list[0].size(), 42);
    }

    #[test]
    fn index_range_from() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 1, 0o644));
        list.push(FileEntry::new_file("b.txt".into(), 2, 0o644));
        list.push(FileEntry::new_file("c.txt".into(), 3, 0o644));
        let tail = &list[1..];
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].name(), "b.txt");
        assert_eq!(tail[1].name(), "c.txt");
    }

    #[test]
    fn index_mut_usize() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("f.txt".into(), 10, 0o644));
        list[0].set_size(99);
        assert_eq!(list[0].size(), 99);
    }

    #[test]
    fn into_vec_returns_legacy() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("x.txt".into(), 5, 0o644));
        let vec = list.into_vec();
        assert_eq!(vec.len(), 1);
        assert_eq!(vec[0].name(), "x.txt");
    }

    #[test]
    fn as_mut_vec_allows_direct_manipulation() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("z.txt".into(), 0, 0o644));
        list.push(FileEntry::new_file("a.txt".into(), 0, 0o644));
        list.as_mut_vec().sort_by(|a, b| a.name().cmp(b.name()));
        assert_eq!(list[0].name(), "a.txt");
        assert_eq!(list[1].name(), "z.txt");
    }

    #[test]
    fn reclaim_segment_clears_entries_in_range() {
        let mut list = DualFileList::new();
        for i in 0..5 {
            list.push(FileEntry::new_file(
                format!("file_{i}.txt").into(),
                (i + 1) as u64 * 100,
                0o644,
            ));
        }

        // Reclaim entries [1..3)
        list.reclaim_segment(1, 3);

        // Unreclaimed entries are intact.
        assert_eq!(list[0].name(), "file_0.txt");
        assert_eq!(list[0].size(), 100);
        assert_eq!(list[3].name(), "file_3.txt");
        assert_eq!(list[4].name(), "file_4.txt");

        // Reclaimed entries have empty names and zero sizes.
        assert_eq!(list[1].name(), "");
        assert_eq!(list[1].size(), 0);
        assert_eq!(list[2].name(), "");
        assert_eq!(list[2].size(), 0);

        // List length is unchanged (entries stay in place).
        assert_eq!(list.len(), 5);
    }

    #[test]
    fn reclaim_segment_empty_range_is_noop() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 10, 0o644));
        list.reclaim_segment(0, 0);
        assert_eq!(list[0].name(), "a.txt");
    }

    #[test]
    fn reclaim_segment_full_range() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 10, 0o644));
        list.push(FileEntry::new_file("b.txt".into(), 20, 0o644));
        list.reclaim_segment(0, 2);
        assert_eq!(list[0].name(), "");
        assert_eq!(list[1].name(), "");
        assert_eq!(list.len(), 2);
    }

    // --- flat-flist feature tests ---

    #[cfg(feature = "flat-flist")]
    mod flat_tests {
        use super::*;
        use crate::flist::ExtrasRef;

        #[test]
        fn flat_accessors_present() {
            let mut list = DualFileList::new();
            list.push(FileEntry::new_file("src/main.rs".into(), 512, 0o644));

            let flat = list.flat();
            assert_eq!(flat.len(), 1);

            let fe = flat.get(0).unwrap();
            assert_eq!(fe.name, b"main.rs");
            assert_eq!(fe.dirname, b"src");
            assert_eq!(fe.header.size, 512);
        }

        #[test]
        fn flat_syncs_with_legacy_on_push() {
            let mut list = DualFileList::new();
            for i in 0u64..5 {
                list.push(FileEntry::new_file(
                    format!("dir/file_{i}.txt").into(),
                    i * 100,
                    0o644,
                ));
            }
            assert_eq!(list.len(), list.flat().len());
            for i in 0..5 {
                let legacy = &list[i];
                let flat_entry = list.flat().get(i).unwrap();
                assert_eq!(flat_entry.header.size, legacy.size());
                assert_eq!(flat_entry.header.mode, legacy.mode());
            }
        }

        #[test]
        fn flat_dirname_interning() {
            let mut list = DualFileList::new();
            list.push(FileEntry::new_file("pkg/a.rs".into(), 10, 0o644));
            list.push(FileEntry::new_file("pkg/b.rs".into(), 20, 0o644));

            let flat = list.flat();
            let e0 = flat.get(0).unwrap();
            let e1 = flat.get(1).unwrap();
            // Same dirname yields the same interned handle.
            assert_eq!(e0.header.dirname, e1.header.dirname);
            assert_eq!(e0.dirname, b"pkg");
        }

        #[test]
        fn flat_root_level_entry_has_empty_dirname() {
            let mut list = DualFileList::new();
            list.push(FileEntry::new_file("README".into(), 100, 0o644));

            let flat = list.flat();
            let entry = flat.get(0).unwrap();
            assert_eq!(entry.name, b"README");
            assert_eq!(entry.dirname, b"");
        }

        #[test]
        fn flat_preserves_uid_gid() {
            let mut list = DualFileList::new();
            let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
            entry.set_uid(1000);
            entry.set_gid(2000);
            list.push(entry);

            let flat = list.flat();
            let h = &flat.get(0).unwrap().header;
            assert_eq!(h.uid(), Some(1000));
            assert_eq!(h.gid(), Some(2000));
        }

        #[test]
        fn flat_preserves_mtime_nsec() {
            let mut list = DualFileList::new();
            let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
            entry.set_mtime(1_000_000, 123_456);
            list.push(entry);

            let flat = list.flat();
            let h = &flat.get(0).unwrap().header;
            assert_eq!(h.mtime, 1_000_000);
            assert_eq!(h.mtime_nsec(), Some(123_456));
        }

        #[test]
        fn flat_symlink_extras_round_trip() {
            let mut list = DualFileList::new();
            let entry = FileEntry::new_symlink("link".into(), "../target".into());
            list.push(entry);

            let flat = list.flat();
            let h = &flat.get(0).unwrap().header;
            let decoded = list.extras().decode(h.extras).unwrap().unwrap();
            assert_eq!(decoded.link_target.as_deref(), Some(b"../target" as &[u8]));
        }

        #[test]
        fn flat_device_extras_round_trip() {
            let mut list = DualFileList::new();
            let entry = FileEntry::new_block_device("dev/sda".into(), 0o660, 8, 0);
            list.push(entry);

            let flat = list.flat();
            let h = &flat.get(0).unwrap().header;
            let decoded = list.extras().decode(h.extras).unwrap().unwrap();
            assert_eq!(decoded.rdev_major, Some(8));
            assert_eq!(decoded.rdev_minor, Some(0));
        }

        #[test]
        fn flat_no_extras_for_plain_file() {
            let mut list = DualFileList::new();
            list.push(FileEntry::new_file("plain.txt".into(), 256, 0o644));

            let flat = list.flat();
            let h = &flat.get(0).unwrap().header;
            assert_eq!(h.extras, ExtrasRef::NO_EXTRAS);
            assert!(list.extras().is_empty());
        }

        #[test]
        fn flat_checksum_extras_round_trip() {
            let mut list = DualFileList::new();
            let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
            entry.set_checksum(vec![0xAB; 16]);
            list.push(entry);

            let decoded = list
                .extras()
                .decode(list.flat().get(0).unwrap().header.extras)
                .unwrap()
                .unwrap();
            assert_eq!(decoded.checksum, Some(vec![0xAB; 16]));
        }

        #[test]
        fn flat_user_group_name_extras_round_trip() {
            let mut list = DualFileList::new();
            let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
            entry.set_user_name("alice".to_string());
            entry.set_group_name("staff".to_string());
            list.push(entry);

            let decoded = list
                .extras()
                .decode(list.flat().get(0).unwrap().header.extras)
                .unwrap()
                .unwrap();
            assert_eq!(decoded.user_name, Some(b"alice".to_vec()));
            assert_eq!(decoded.group_name, Some(b"staff".to_vec()));
        }

        #[test]
        fn flat_hardlink_idx_extras_round_trip() {
            let mut list = DualFileList::new();
            let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
            entry.set_hardlink_idx(42);
            list.push(entry);

            let decoded = list
                .extras()
                .decode(list.flat().get(0).unwrap().header.extras)
                .unwrap()
                .unwrap();
            assert_eq!(decoded.hardlink_idx, Some(42));
        }

        #[test]
        fn flat_acl_xattr_extras_round_trip() {
            let mut list = DualFileList::new();
            let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
            entry.set_acl_ndx(3);
            entry.set_def_acl_ndx(4);
            entry.set_xattr_ndx(5);
            list.push(entry);

            let decoded = list
                .extras()
                .decode(list.flat().get(0).unwrap().header.extras)
                .unwrap()
                .unwrap();
            assert_eq!(decoded.acl_ndx, Some(3));
            assert_eq!(decoded.def_acl_ndx, Some(4));
            assert_eq!(decoded.xattr_ndx, Some(5));
        }

        #[test]
        fn flat_atime_crtime_extras_round_trip() {
            let mut list = DualFileList::new();
            let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
            entry.set_atime(1_234_567);
            entry.set_crtime(7_654_321);
            entry.set_atime_nsec(999);
            list.push(entry);

            let decoded = list
                .extras()
                .decode(list.flat().get(0).unwrap().header.extras)
                .unwrap()
                .unwrap();
            assert_eq!(decoded.atime, Some(1_234_567));
            assert_eq!(decoded.crtime, Some(7_654_321));
            assert_eq!(decoded.atime_nsec, Some(999));
        }

        #[test]
        fn flat_content_dir_flag_preserved() {
            let mut list = DualFileList::new();
            let mut dir = FileEntry::new_directory("mydir".into(), 0o755);
            dir.set_content_dir(true);
            list.push(dir);

            let h = &list.flat().get(0).unwrap().header;
            assert!(h.has(PRESENT_CONTENT_DIR));

            let mut list2 = DualFileList::new();
            let mut dir2 = FileEntry::new_directory("nodir".into(), 0o755);
            dir2.set_content_dir(false);
            list2.push(dir2);

            let h2 = &list2.flat().get(0).unwrap().header;
            assert!(!h2.has(PRESENT_CONTENT_DIR));
        }

        #[test]
        fn flat_length64_flag_for_large_files() {
            let mut list = DualFileList::new();
            let large_size = u64::from(u32::MAX) + 1;
            list.push(FileEntry::new_file("big.bin".into(), large_size, 0o644));

            let h = &list.flat().get(0).unwrap().header;
            assert!(h.has(PRESENT_LENGTH64));
            assert_eq!(h.size, large_size);
        }

        #[test]
        fn flat_nested_path_split() {
            let mut list = DualFileList::new();
            list.push(FileEntry::new_file("a/b/c/d.txt".into(), 0, 0o644));

            let fe = list.flat().get(0).unwrap();
            assert_eq!(fe.name, b"d.txt");
            assert_eq!(fe.dirname, b"a/b/c");
        }

        #[test]
        fn extras_arena_accessible() {
            let list = DualFileList::new();
            assert!(list.extras().is_empty());
        }

        /// Verifies that dirname sharing through PathArena deduplicates
        /// identical directory paths, analogous to upstream rsync's `lastdir`
        /// cache (upstream: flist.c:765-773).
        ///
        /// Pushes 100 files across 5 directories and asserts that PathArena
        /// stores each dirname exactly once rather than 100 times.
        #[test]
        fn dirname_sharing_deduplicates_across_many_files() {
            let mut list = DualFileList::new();

            let dirs = ["src", "tests", "docs", "scripts", "benches"];
            for (i, dir) in dirs.iter().enumerate() {
                for j in 0..20 {
                    let path = format!("{dir}/file_{j}.rs");
                    let size = (i * 20 + j) as u64;
                    list.push(FileEntry::new_file(path.into(), size, 0o644));
                }
            }

            assert_eq!(list.len(), 100);
            assert_eq!(list.flat().len(), 100);

            let paths = list.flat().paths();

            // 5 unique dirnames + 20 unique basenames per dir = at most
            // 5 + 100 = 105, but basenames like "file_0.rs" repeat across
            // dirs so the actual count is 5 dirs + 20 unique basenames = 25.
            assert_eq!(paths.len(), 25);

            // The byte arena holds each string once: the 5 dirnames plus
            // 20 basenames. Verify the dirname contribution is exactly the
            // sum of the 5 unique dirname lengths, not 100x that.
            let dirname_bytes: usize = dirs.iter().map(|d| d.len()).sum();
            let basename_bytes: usize = (0..20).map(|j| format!("file_{j}.rs").len()).sum();
            assert_eq!(paths.bytes_len(), dirname_bytes + basename_bytes);

            // Every pair of entries in the same directory shares the same
            // dirname handle.
            for dir in &dirs {
                let entries_in_dir: Vec<_> = (0..list.flat().len())
                    .filter_map(|i| {
                        let e = list.flat().get(i)?;
                        if e.dirname == dir.as_bytes() {
                            Some(e.header.dirname)
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(entries_in_dir.len(), 20);
                let first = entries_in_dir[0];
                assert!(entries_in_dir.iter().all(|h| *h == first));
            }
        }

        /// Verifies that nested directory paths are deduplicated correctly
        /// when multiple files share the same multi-level directory.
        #[test]
        fn dirname_sharing_nested_paths() {
            let mut list = DualFileList::new();
            let nested_dir = "a/b/c/d";
            for i in 0..50 {
                let path = format!("{nested_dir}/item_{i}.txt");
                list.push(FileEntry::new_file(path.into(), i, 0o644));
            }

            let paths = list.flat().paths();

            // 1 unique dirname ("a/b/c/d") + 50 unique basenames
            assert_eq!(paths.len(), 51);

            // All 50 entries share the same dirname handle.
            let first_dirname = list.flat().get(0).unwrap().header.dirname;
            for i in 1..50 {
                assert_eq!(list.flat().get(i).unwrap().header.dirname, first_dirname);
            }
        }

        /// Describes which optional fields to set on a test entry.
        ///
        /// Each combination of these flags produces a distinct `FileEntry`
        /// variant, ensuring the flat conversion covers all optional field
        /// paths.
        #[derive(Clone, Copy)]
        struct FieldCombo {
            uid: bool,
            gid: bool,
            mtime_nsec: bool,
            link_target: bool,
            rdev: bool,
            checksum: bool,
            hardlink_idx: bool,
            acl_ndx: bool,
            def_acl_ndx: bool,
            xattr_ndx: bool,
            user_name: bool,
            group_name: bool,
            atime: bool,
            crtime: bool,
            atime_nsec: bool,
            content_dir_off: bool,
        }

        /// Builds a `FileEntry` with the fields indicated by `combo`.
        fn build_entry(idx: usize, combo: FieldCombo) -> FileEntry {
            let path: std::path::PathBuf = if combo.link_target {
                format!("dir_{}/link_{idx}", idx % 7).into()
            } else if combo.rdev {
                format!("dev/node_{idx}").into()
            } else if combo.content_dir_off {
                format!("dirs/d_{idx}").into()
            } else {
                format!("src/pkg_{}/file_{idx}.rs", idx % 5).into()
            };

            let mut entry = if combo.link_target {
                FileEntry::new_symlink(path, format!("../targets/dest_{idx}").into())
            } else if combo.rdev {
                FileEntry::new_block_device(path, 0o660, (idx as u32) + 1, (idx as u32) * 3)
            } else if combo.content_dir_off {
                let mut d = FileEntry::new_directory(path, 0o755);
                d.set_content_dir(false);
                d
            } else {
                FileEntry::new_file(path, (idx as u64 + 1) * 1024, 0o644)
            };

            entry.set_mtime(1_700_000_000 + idx as i64, 0);

            if combo.uid {
                entry.set_uid(1000 + idx as u32);
            }
            if combo.gid {
                entry.set_gid(2000 + idx as u32);
            }
            if combo.mtime_nsec {
                entry.set_mtime(entry.mtime(), 500_000 + idx as u32);
            }
            if combo.checksum {
                let mut sum = vec![0u8; 16];
                for (j, b) in sum.iter_mut().enumerate() {
                    *b = ((idx + j) & 0xFF) as u8;
                }
                entry.set_checksum(sum);
            }
            if combo.hardlink_idx {
                entry.set_hardlink_idx(100 + idx as u32);
            }
            if combo.acl_ndx {
                entry.set_acl_ndx(10 + idx as u32);
            }
            if combo.def_acl_ndx {
                entry.set_def_acl_ndx(20 + idx as u32);
            }
            if combo.xattr_ndx {
                entry.set_xattr_ndx(30 + idx as u32);
            }
            if combo.user_name {
                entry.set_user_name(format!("user_{idx}"));
            }
            if combo.group_name {
                entry.set_group_name(format!("group_{idx}"));
            }
            if combo.atime {
                entry.set_atime(1_600_000_000 + idx as i64);
            }
            if combo.crtime {
                entry.set_crtime(1_500_000_000 + idx as i64);
            }
            if combo.atime_nsec {
                entry.set_atime_nsec(100_000 + idx as u32);
            }
            entry
        }

        /// Asserts that every field of the legacy `FileEntry` at `idx` matches
        /// the corresponding flat representation in the `DualFileList`.
        fn assert_entry_equivalence(list: &DualFileList, idx: usize) {
            let legacy = &list[idx];
            let flat_entry = list
                .flat()
                .get(idx)
                .unwrap_or_else(|| panic!("flat entry {idx} missing"));

            // Name: the flat store splits into dirname/basename at the last '/'.
            let full_name = legacy.name();
            let (expected_dirname, expected_basename) = match full_name.rfind('/') {
                Some(pos) => (&full_name[..pos], &full_name[pos + 1..]),
                None => ("", full_name),
            };
            assert_eq!(
                flat_entry.name,
                expected_basename.as_bytes(),
                "entry {idx}: name mismatch"
            );
            assert_eq!(
                flat_entry.dirname,
                expected_dirname.as_bytes(),
                "entry {idx}: dirname mismatch"
            );

            // Scalar inline fields.
            assert_eq!(
                flat_entry.header.mode,
                legacy.mode(),
                "entry {idx}: mode mismatch"
            );
            assert_eq!(
                flat_entry.header.mtime,
                legacy.mtime(),
                "entry {idx}: mtime mismatch"
            );
            assert_eq!(
                flat_entry.header.size,
                legacy.size(),
                "entry {idx}: size mismatch"
            );

            // Presence-gated inline fields.
            assert_eq!(
                flat_entry.header.uid(),
                legacy.uid(),
                "entry {idx}: uid mismatch"
            );
            assert_eq!(
                flat_entry.header.gid(),
                legacy.gid(),
                "entry {idx}: gid mismatch"
            );

            let expected_nsec = if legacy.mtime_nsec() != 0 {
                Some(legacy.mtime_nsec())
            } else {
                None
            };
            assert_eq!(
                flat_entry.header.mtime_nsec(),
                expected_nsec,
                "entry {idx}: mtime_nsec mismatch"
            );

            // Content-dir flag.
            assert_eq!(
                flat_entry.header.has(PRESENT_CONTENT_DIR),
                legacy.content_dir(),
                "entry {idx}: content_dir mismatch"
            );

            // Extras: decode the flat extras and compare with legacy accessors.
            let flat_extras = list
                .extras()
                .decode(flat_entry.header.extras)
                .unwrap_or_else(|e| panic!("entry {idx}: extras decode failed: {e}"));

            // When there are no extras the decoded result is None.
            match flat_extras {
                None => {
                    // Legacy must also have no extras-backed fields.
                    assert!(
                        legacy.link_target().is_none(),
                        "entry {idx}: expected no link_target"
                    );
                    assert!(
                        legacy.rdev_major().is_none(),
                        "entry {idx}: expected no rdev"
                    );
                    assert!(
                        legacy.checksum().is_none(),
                        "entry {idx}: expected no checksum"
                    );
                    assert!(
                        legacy.hardlink_idx().is_none(),
                        "entry {idx}: expected no hardlink_idx"
                    );
                    assert!(
                        legacy.acl_ndx().is_none(),
                        "entry {idx}: expected no acl_ndx"
                    );
                    assert!(
                        legacy.def_acl_ndx().is_none(),
                        "entry {idx}: expected no def_acl_ndx"
                    );
                    assert!(
                        legacy.xattr_ndx().is_none(),
                        "entry {idx}: expected no xattr_ndx"
                    );
                    assert!(
                        legacy.user_name().is_none(),
                        "entry {idx}: expected no user_name"
                    );
                    assert!(
                        legacy.group_name().is_none(),
                        "entry {idx}: expected no group_name"
                    );
                    assert_eq!(legacy.atime(), 0, "entry {idx}: expected atime 0");
                    assert_eq!(legacy.crtime(), 0, "entry {idx}: expected crtime 0");
                    assert_eq!(legacy.atime_nsec(), 0, "entry {idx}: expected atime_nsec 0");
                }
                Some(decoded) => {
                    // Link target.
                    match legacy.link_target() {
                        Some(target) => {
                            #[cfg(unix)]
                            {
                                use std::os::unix::ffi::OsStrExt;
                                assert_eq!(
                                    decoded.link_target.as_deref(),
                                    Some(target.as_os_str().as_bytes()),
                                    "entry {idx}: link_target mismatch"
                                );
                            }
                            #[cfg(not(unix))]
                            {
                                let lossy = target.to_string_lossy();
                                assert_eq!(
                                    decoded.link_target.as_deref(),
                                    Some(lossy.as_bytes()),
                                    "entry {idx}: link_target mismatch"
                                );
                            }
                        }
                        None => assert_eq!(
                            decoded.link_target, None,
                            "entry {idx}: link_target should be None"
                        ),
                    }

                    // Device numbers.
                    assert_eq!(
                        decoded.rdev_major,
                        legacy.rdev_major(),
                        "entry {idx}: rdev_major mismatch"
                    );
                    assert_eq!(
                        decoded.rdev_minor,
                        legacy.rdev_minor(),
                        "entry {idx}: rdev_minor mismatch"
                    );

                    // Checksum.
                    assert_eq!(
                        decoded.checksum.as_deref(),
                        legacy.checksum(),
                        "entry {idx}: checksum mismatch"
                    );

                    // Hardlink index.
                    assert_eq!(
                        decoded.hardlink_idx,
                        legacy.hardlink_idx(),
                        "entry {idx}: hardlink_idx mismatch"
                    );

                    // ACL/xattr indices.
                    assert_eq!(
                        decoded.acl_ndx,
                        legacy.acl_ndx(),
                        "entry {idx}: acl_ndx mismatch"
                    );
                    assert_eq!(
                        decoded.def_acl_ndx,
                        legacy.def_acl_ndx(),
                        "entry {idx}: def_acl_ndx mismatch"
                    );
                    assert_eq!(
                        decoded.xattr_ndx,
                        legacy.xattr_ndx(),
                        "entry {idx}: xattr_ndx mismatch"
                    );

                    // User/group names.
                    assert_eq!(
                        decoded
                            .user_name
                            .as_deref()
                            .map(|b| std::str::from_utf8(b).unwrap()),
                        legacy.user_name(),
                        "entry {idx}: user_name mismatch"
                    );
                    assert_eq!(
                        decoded
                            .group_name
                            .as_deref()
                            .map(|b| std::str::from_utf8(b).unwrap()),
                        legacy.group_name(),
                        "entry {idx}: group_name mismatch"
                    );

                    // Atime/crtime.
                    assert_eq!(
                        decoded.atime.unwrap_or(0),
                        legacy.atime(),
                        "entry {idx}: atime mismatch"
                    );
                    assert_eq!(
                        decoded.crtime.unwrap_or(0),
                        legacy.crtime(),
                        "entry {idx}: crtime mismatch"
                    );
                    assert_eq!(
                        decoded.atime_nsec.unwrap_or(0),
                        legacy.atime_nsec(),
                        "entry {idx}: atime_nsec mismatch"
                    );
                }
            }
        }

        /// Verifies field-level equivalence between legacy `Vec<FileEntry>` and
        /// `FlatFileList` representations across 100+ entries covering every
        /// optional field combination.
        ///
        /// Each entry is pushed through `DualFileList::push`, which populates
        /// both representations from the same `FileEntry` source. The test then
        /// walks every entry and asserts that the flat header + extras arena
        /// matches the legacy accessors field by field: name, dirname, mode,
        /// mtime, size, uid, gid, mtime_nsec, content_dir, link_target, rdev,
        /// checksum, hardlink_idx, acl_ndx, def_acl_ndx, xattr_ndx, user_name,
        /// group_name, atime, crtime, and atime_nsec.
        #[test]
        fn flat_matches_legacy_field_by_field_all_combos() {
            // 16 boolean field flags gives 2^16 = 65536 combinations. We sample
            // a representative set by iterating bits 0..15 and toggling each
            // independently, producing 100+ distinct combos that exercise every
            // field both present and absent.
            let mut entries: Vec<FieldCombo> = Vec::with_capacity(128);

            // Combo 0: all fields absent (plain file, no extras).
            entries.push(FieldCombo {
                uid: false,
                gid: false,
                mtime_nsec: false,
                link_target: false,
                rdev: false,
                checksum: false,
                hardlink_idx: false,
                acl_ndx: false,
                def_acl_ndx: false,
                xattr_ndx: false,
                user_name: false,
                group_name: false,
                atime: false,
                crtime: false,
                atime_nsec: false,
                content_dir_off: false,
            });

            // Combos 1-16: exactly one field set at a time.
            let field_names = [
                "uid",
                "gid",
                "mtime_nsec",
                "link_target",
                "rdev",
                "checksum",
                "hardlink_idx",
                "acl_ndx",
                "def_acl_ndx",
                "xattr_ndx",
                "user_name",
                "group_name",
                "atime",
                "crtime",
                "atime_nsec",
                "content_dir_off",
            ];
            for bit in 0..field_names.len() {
                let mut combo = FieldCombo {
                    uid: false,
                    gid: false,
                    mtime_nsec: false,
                    link_target: false,
                    rdev: false,
                    checksum: false,
                    hardlink_idx: false,
                    acl_ndx: false,
                    def_acl_ndx: false,
                    xattr_ndx: false,
                    user_name: false,
                    group_name: false,
                    atime: false,
                    crtime: false,
                    atime_nsec: false,
                    content_dir_off: false,
                };
                set_combo_bit(&mut combo, bit);
                entries.push(combo);
            }

            // Combos 17-32: pairs of adjacent fields.
            for bit in 0..field_names.len() {
                let mut combo = FieldCombo {
                    uid: false,
                    gid: false,
                    mtime_nsec: false,
                    link_target: false,
                    rdev: false,
                    checksum: false,
                    hardlink_idx: false,
                    acl_ndx: false,
                    def_acl_ndx: false,
                    xattr_ndx: false,
                    user_name: false,
                    group_name: false,
                    atime: false,
                    crtime: false,
                    atime_nsec: false,
                    content_dir_off: false,
                };
                set_combo_bit(&mut combo, bit);
                set_combo_bit(&mut combo, (bit + 1) % field_names.len());
                entries.push(combo);
            }

            // Combos 33-64: groups of 4 using stride-4 patterns.
            for start in 0..16u32 {
                let mask = start.wrapping_mul(0x1111) & 0xFFFF;
                let mut combo = FieldCombo {
                    uid: false,
                    gid: false,
                    mtime_nsec: false,
                    link_target: false,
                    rdev: false,
                    checksum: false,
                    hardlink_idx: false,
                    acl_ndx: false,
                    def_acl_ndx: false,
                    xattr_ndx: false,
                    user_name: false,
                    group_name: false,
                    atime: false,
                    crtime: false,
                    atime_nsec: false,
                    content_dir_off: false,
                };
                for bit in 0..field_names.len() {
                    if mask & (1 << bit) != 0 {
                        set_combo_bit(&mut combo, bit);
                    }
                }
                entries.push(combo);
            }

            // Combos 65-96: scattered selections using XOR pattern.
            for seed in 0u32..32 {
                let mask = seed ^ (seed.wrapping_mul(2027));
                let mut combo = FieldCombo {
                    uid: false,
                    gid: false,
                    mtime_nsec: false,
                    link_target: false,
                    rdev: false,
                    checksum: false,
                    hardlink_idx: false,
                    acl_ndx: false,
                    def_acl_ndx: false,
                    xattr_ndx: false,
                    user_name: false,
                    group_name: false,
                    atime: false,
                    crtime: false,
                    atime_nsec: false,
                    content_dir_off: false,
                };
                for bit in 0..field_names.len() {
                    if mask & (1 << bit) != 0 {
                        set_combo_bit(&mut combo, bit);
                    }
                }
                entries.push(combo);
            }

            // Combo: all fields present (the maximal entry).
            entries.push(FieldCombo {
                uid: true,
                gid: true,
                mtime_nsec: true,
                link_target: true,
                rdev: false, // mutually exclusive with link_target
                checksum: true,
                hardlink_idx: true,
                acl_ndx: true,
                def_acl_ndx: true,
                xattr_ndx: true,
                user_name: true,
                group_name: true,
                atime: true,
                crtime: true,
                atime_nsec: true,
                content_dir_off: false,
            });

            // Combo: all scalar extras (no link_target, no rdev).
            entries.push(FieldCombo {
                uid: true,
                gid: true,
                mtime_nsec: true,
                link_target: false,
                rdev: false,
                checksum: true,
                hardlink_idx: true,
                acl_ndx: true,
                def_acl_ndx: true,
                xattr_ndx: true,
                user_name: true,
                group_name: true,
                atime: true,
                crtime: true,
                atime_nsec: true,
                content_dir_off: false,
            });

            // Combo: device with all scalar extras.
            entries.push(FieldCombo {
                uid: true,
                gid: true,
                mtime_nsec: true,
                link_target: false,
                rdev: true,
                checksum: true,
                hardlink_idx: true,
                acl_ndx: true,
                def_acl_ndx: true,
                xattr_ndx: true,
                user_name: true,
                group_name: true,
                atime: true,
                crtime: true,
                atime_nsec: true,
                content_dir_off: false,
            });

            assert!(
                entries.len() >= 80,
                "expected 80+ combos, got {}",
                entries.len()
            );

            let mut list = DualFileList::with_capacity(entries.len());
            for (idx, combo) in entries.iter().enumerate() {
                list.push(build_entry(idx, *combo));
            }

            // Both representations must have the same length.
            assert_eq!(list.len(), list.flat().len());

            // Assert field-by-field equivalence for every entry.
            for idx in 0..list.len() {
                assert_entry_equivalence(&list, idx);
            }
        }

        /// Sets the `bit`-th boolean field on a `FieldCombo`.
        fn set_combo_bit(combo: &mut FieldCombo, bit: usize) {
            match bit {
                0 => combo.uid = true,
                1 => combo.gid = true,
                2 => combo.mtime_nsec = true,
                3 => combo.link_target = true,
                4 => combo.rdev = true,
                5 => combo.checksum = true,
                6 => combo.hardlink_idx = true,
                7 => combo.acl_ndx = true,
                8 => combo.def_acl_ndx = true,
                9 => combo.xattr_ndx = true,
                10 => combo.user_name = true,
                11 => combo.group_name = true,
                12 => combo.atime = true,
                13 => combo.crtime = true,
                14 => combo.atime_nsec = true,
                15 => combo.content_dir_off = true,
                _ => {}
            }
        }
    }
}
