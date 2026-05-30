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
            self.extras = ExtrasArena::new();
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

    let flags = entry.flags();
    let flags_u16 = u16::from(flags.primary) | (u16::from(flags.extended) << 8);

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
    }
}
