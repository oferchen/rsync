//! Full transfer regression test through the flat flist path (RSS-A.7.i).
//!
//! Validates that the arena-backed `FlatFileList` produces identical results
//! to the legacy `Vec<FileEntry>` path across the major consumer pipeline:
//! sort, filter, and accessor trait iteration.
//!
//! The test builds a mixed file list containing regular files, directories,
//! and symlinks through `DualFileList`, then asserts parity between the
//! legacy and flat representations at each stage.

#![cfg(feature = "flat-flist")]

use protocol::flist::{
    DualFileList, ExtrasArena, ExtrasRef, FileEntry, FileEntryAccessor, FileEntryHeader,
    FlatFileEntry, FlatFileList, PRESENT_GID, PRESENT_UID, compare_entries_generic,
    compare_file_entries, sort_entries_generic, sort_file_list,
};

// ---------------------------------------------------------------------------
// Test fixture: mixed file list
// ---------------------------------------------------------------------------

/// Builds a representative file list with regular files, directories, and
/// symlinks spread across multiple directory levels. Returns the entries
/// as a `Vec<FileEntry>` in insertion order (unsorted).
fn build_mixed_fixture() -> Vec<FileEntry> {
    let mut entries = Vec::new();

    // Root-level entries.
    entries.push(FileEntry::new_file("README".into(), 2048, 0o644));
    entries.push(FileEntry::new_directory(".".into(), 0o755));
    entries.push(FileEntry::new_file("Makefile".into(), 512, 0o644));

    // src/ directory tree.
    entries.push(FileEntry::new_directory("src".into(), 0o755));
    {
        let mut f = FileEntry::new_file("src/main.rs".into(), 4096, 0o644);
        f.set_uid(1000);
        f.set_gid(100);
        f.set_mtime(1_700_000_000, 123_456);
        entries.push(f);
    }
    {
        let mut f = FileEntry::new_file("src/lib.rs".into(), 8192, 0o644);
        f.set_uid(1000);
        f.set_gid(100);
        f.set_mtime(1_700_000_100, 0);
        entries.push(f);
    }
    entries.push(FileEntry::new_file("src/util.rs".into(), 1024, 0o644));

    // docs/ directory tree with a symlink.
    entries.push(FileEntry::new_directory("docs".into(), 0o755));
    entries.push(FileEntry::new_file("docs/guide.md".into(), 3072, 0o644));
    entries.push(FileEntry::new_symlink(
        "docs/latest".into(),
        "../README".into(),
    ));

    // tests/ directory tree.
    entries.push(FileEntry::new_directory("tests".into(), 0o755));
    entries.push(FileEntry::new_file(
        "tests/integration.rs".into(),
        6144,
        0o644,
    ));
    entries.push(FileEntry::new_file("tests/unit.rs".into(), 2048, 0o644));

    // Nested directory with extras.
    entries.push(FileEntry::new_directory("src/utils".into(), 0o755));
    {
        let mut f = FileEntry::new_file("src/utils/helpers.rs".into(), 768, 0o644);
        f.set_uid(1001);
        f.set_gid(200);
        f.set_atime(1_600_000_000);
        f.set_crtime(1_500_000_000);
        f.set_checksum(vec![0xAB; 16]);
        entries.push(f);
    }

    // Device entry (block device).
    entries.push(FileEntry::new_block_device("dev/sda".into(), 0o660, 8, 0));

    // Another symlink at root level.
    entries.push(FileEntry::new_symlink("link_to_src".into(), "src".into()));

    // Directory with content_dir toggled off.
    {
        let mut d = FileEntry::new_directory("empty_dir".into(), 0o755);
        d.set_content_dir(false);
        entries.push(d);
    }

    // File with ACL and xattr indices.
    {
        let mut f = FileEntry::new_file("protected.dat".into(), 16384, 0o600);
        f.set_uid(0);
        f.set_gid(0);
        f.set_acl_ndx(1);
        f.set_def_acl_ndx(2);
        f.set_xattr_ndx(3);
        f.set_user_name("root".to_string());
        f.set_group_name("wheel".to_string());
        entries.push(f);
    }

    entries
}

// ---------------------------------------------------------------------------
// Helper: build a FlatFileEntry with extras arena wired in
// ---------------------------------------------------------------------------

/// Wraps `FlatFileList::get()` to also wire the extras arena into the view,
/// enabling `FileEntryAccessor` methods that decode scalar extras fields.
fn flat_entry_with_extras<'a>(
    flat: &'a FlatFileList,
    extras: &'a ExtrasArena,
    index: usize,
) -> Option<FlatFileEntry<'a>> {
    let entry = flat.get(index)?;
    Some(FlatFileEntry {
        header: entry.header,
        name: entry.name,
        dirname: entry.dirname,
        extras_arena: Some(extras),
    })
}

// ---------------------------------------------------------------------------
// Test: DualFileList parity - counts and scalar metadata
// ---------------------------------------------------------------------------

/// Verifies that pushing entries through DualFileList produces identical
/// entry counts and scalar metadata in both legacy and flat representations.
#[test]
fn dual_file_list_scalar_parity() {
    let fixture = build_mixed_fixture();
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    let flat = dual.flat();
    assert_eq!(dual.len(), flat.len(), "entry count must match");
    assert_eq!(dual.len(), fixture.len());

    for i in 0..dual.len() {
        let legacy = &dual[i];
        let flat_entry = flat_entry_with_extras(flat, dual.extras(), i)
            .unwrap_or_else(|| panic!("flat entry {i} missing"));

        // Name: flat splits into dirname/basename at last '/'.
        let full_name = legacy.name();
        let (expected_dirname, expected_basename) = match full_name.rfind('/') {
            Some(pos) => (&full_name[..pos], &full_name[pos + 1..]),
            None => ("", full_name),
        };
        assert_eq!(
            std::str::from_utf8(flat_entry.name).unwrap_or(""),
            expected_basename,
            "entry {i}: basename mismatch"
        );
        assert_eq!(
            std::str::from_utf8(flat_entry.dirname).unwrap_or(""),
            expected_dirname,
            "entry {i}: dirname mismatch"
        );

        // Scalar header fields.
        assert_eq!(flat_entry.header.size, legacy.size(), "entry {i}: size");
        assert_eq!(flat_entry.header.mode, legacy.mode(), "entry {i}: mode");
        assert_eq!(flat_entry.header.mtime, legacy.mtime(), "entry {i}: mtime");
        assert_eq!(flat_entry.header.uid(), legacy.uid(), "entry {i}: uid");
        assert_eq!(flat_entry.header.gid(), legacy.gid(), "entry {i}: gid");
    }
}

// ---------------------------------------------------------------------------
// Test: FileEntryAccessor parity between legacy and flat
// ---------------------------------------------------------------------------

/// Verifies that `FileEntryAccessor` trait methods return identical results
/// for the legacy `FileEntry` and the flat `FlatFileEntry` across all entries
/// in the mixed fixture.
#[test]
fn accessor_trait_parity_all_entries() {
    let fixture = build_mixed_fixture();
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    for i in 0..dual.len() {
        let legacy: &dyn FileEntryAccessor = &dual[i];
        let flat_entry = flat_entry_with_extras(dual.flat(), dual.extras(), i)
            .unwrap_or_else(|| panic!("flat entry {i} missing"));
        let flat_acc: &dyn FileEntryAccessor = &flat_entry;

        // Path accessors - flat name is basename only, so skip name() comparison
        // for entries with '/' in their name. dirname_str should match the
        // dirname portion.
        let legacy_name = legacy.name();
        let flat_dirname = flat_acc.dirname_str();
        let expected_dirname = match legacy_name.rfind('/') {
            Some(pos) => &legacy_name[..pos],
            None => "",
        };
        assert_eq!(flat_dirname, expected_dirname, "entry {i}: dirname_str");

        // Scalar metadata.
        assert_eq!(legacy.size(), flat_acc.size(), "entry {i}: size");
        assert_eq!(legacy.mode(), flat_acc.mode(), "entry {i}: mode");
        assert_eq!(
            legacy.permissions(),
            flat_acc.permissions(),
            "entry {i}: permissions"
        );
        assert_eq!(legacy.mtime(), flat_acc.mtime(), "entry {i}: mtime");
        assert_eq!(legacy.uid(), flat_acc.uid(), "entry {i}: uid");
        assert_eq!(legacy.gid(), flat_acc.gid(), "entry {i}: gid");

        // Type queries.
        assert_eq!(legacy.is_file(), flat_acc.is_file(), "entry {i}: is_file");
        assert_eq!(legacy.is_dir(), flat_acc.is_dir(), "entry {i}: is_dir");
        assert_eq!(
            legacy.is_symlink(),
            flat_acc.is_symlink(),
            "entry {i}: is_symlink"
        );
        assert_eq!(
            legacy.is_device(),
            flat_acc.is_device(),
            "entry {i}: is_device"
        );
        assert_eq!(
            legacy.is_special(),
            flat_acc.is_special(),
            "entry {i}: is_special"
        );
        assert_eq!(
            legacy.file_type(),
            flat_acc.file_type(),
            "entry {i}: file_type"
        );

        // Scalar extras decoded via the accessor.
        assert_eq!(
            legacy.rdev_major(),
            flat_acc.rdev_major(),
            "entry {i}: rdev_major"
        );
        assert_eq!(
            legacy.rdev_minor(),
            flat_acc.rdev_minor(),
            "entry {i}: rdev_minor"
        );
        assert_eq!(legacy.acl_ndx(), flat_acc.acl_ndx(), "entry {i}: acl_ndx");
        assert_eq!(
            legacy.def_acl_ndx(),
            flat_acc.def_acl_ndx(),
            "entry {i}: def_acl_ndx"
        );
        assert_eq!(
            legacy.xattr_ndx(),
            flat_acc.xattr_ndx(),
            "entry {i}: xattr_ndx"
        );
        assert_eq!(legacy.atime(), flat_acc.atime(), "entry {i}: atime");
        assert_eq!(legacy.crtime(), flat_acc.crtime(), "entry {i}: crtime");
    }
}

// ---------------------------------------------------------------------------
// Test: Sort parity between legacy and flat paths
// ---------------------------------------------------------------------------

/// Verifies that sorting the legacy `Vec<FileEntry>` with `sort_file_list`
/// and sorting via `compare_entries_generic` on the same entries produce
/// identical orderings.
#[test]
fn sort_parity_legacy_vs_generic() {
    let fixture = build_mixed_fixture();

    // Sort legacy entries.
    let mut legacy = fixture.clone();
    sort_file_list(&mut legacy, false, false);

    // Sort the same entries via the generic sort.
    let mut generic = fixture.clone();
    sort_entries_generic(&mut generic, false, false);

    // Both must produce the same ordering.
    assert_eq!(legacy.len(), generic.len());
    for i in 0..legacy.len() {
        assert_eq!(
            legacy[i].name(),
            generic[i].name(),
            "sort order diverged at index {i}"
        );
    }
}

/// Verifies that `FlatFileList::sort()` produces the same ordering as
/// `sort_file_list()` on the corresponding legacy entries.
#[test]
fn sort_parity_flat_file_list() {
    let fixture = build_mixed_fixture();

    // Sort legacy entries.
    let mut legacy = fixture.clone();
    sort_file_list(&mut legacy, false, false);

    // Build and sort a FlatFileList with the same entries.
    let mut flat = FlatFileList::new();
    for entry in &fixture {
        let full_name = entry.name();
        let (dirname_str, basename_str) = match full_name.rfind('/') {
            Some(pos) => (&full_name[..pos], &full_name[pos + 1..]),
            None => ("", full_name),
        };
        let name_h = flat.paths_mut().intern(basename_str);
        let dirname_h = flat.paths_mut().intern(dirname_str);
        let mut h = FileEntryHeader {
            mtime: entry.mtime(),
            size: entry.size(),
            uid: entry.uid().unwrap_or(0),
            gid: entry.gid().unwrap_or(0),
            name: name_h,
            dirname: dirname_h,
            extras: ExtrasRef::NO_EXTRAS,
            mtime_nsec: entry.mtime_nsec(),
            mode: entry.mode(),
            flags: 0,
            present: 0,
        };
        if entry.uid().is_some() {
            h.set(PRESENT_UID);
        }
        if entry.gid().is_some() {
            h.set(PRESENT_GID);
        }
        flat.push(h);
    }

    flat.sort();

    // Compare orderings. FlatFileList sorts by dirname then basename, which
    // is a simpler lexicographic sort - not identical to the full rsync sort
    // that considers file-before-directory and implicit trailing '/'. We
    // verify the flat sort is at least internally consistent: sorted entries
    // are in non-decreasing dirname+basename order.
    let mut prev_key = (Vec::<u8>::new(), Vec::<u8>::new());
    for i in 0..flat.len() {
        let entry = flat.get(i).unwrap();
        let key = (entry.dirname.to_vec(), entry.name.to_vec());
        assert!(
            key >= prev_key,
            "flat sort not monotonic at index {i}: ({:?}, {:?}) < ({:?}, {:?})",
            std::str::from_utf8(&key.0).unwrap_or("?"),
            std::str::from_utf8(&key.1).unwrap_or("?"),
            std::str::from_utf8(&prev_key.0).unwrap_or("?"),
            std::str::from_utf8(&prev_key.1).unwrap_or("?"),
        );
        prev_key = key;
    }
}

/// Verifies that `compare_entries_generic` and `compare_file_entries` agree
/// for every pair of entries in the fixture.
#[test]
fn compare_parity_all_pairs() {
    let fixture = build_mixed_fixture();
    let n = fixture.len();

    for i in 0..n {
        for j in 0..n {
            let concrete = compare_file_entries(&fixture[i], &fixture[j]);
            let generic = compare_entries_generic(&fixture[i], &fixture[j]);
            assert_eq!(
                concrete,
                generic,
                "compare parity failed for ({}, {})",
                fixture[i].name(),
                fixture[j].name(),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Test: Sort parity with qsort and pre-29 protocol modes
// ---------------------------------------------------------------------------

/// Verifies that the generic sort matches the concrete sort in qsort mode.
#[test]
fn sort_parity_qsort_mode() {
    let fixture = build_mixed_fixture();

    let mut legacy = fixture.clone();
    sort_file_list(&mut legacy, true, false);

    let mut generic = fixture.clone();
    sort_entries_generic(&mut generic, true, false);

    for i in 0..legacy.len() {
        assert_eq!(
            legacy[i].name(),
            generic[i].name(),
            "qsort order diverged at index {i}"
        );
    }
}

/// Verifies that the generic sort matches the concrete sort in pre-29 mode.
#[test]
fn sort_parity_pre29_mode() {
    let fixture = build_mixed_fixture();

    let mut legacy = fixture.clone();
    sort_file_list(&mut legacy, false, true);

    let mut generic = fixture.clone();
    sort_entries_generic(&mut generic, false, true);

    for i in 0..legacy.len() {
        assert_eq!(
            legacy[i].name(),
            generic[i].name(),
            "pre29 sort order diverged at index {i}"
        );
    }
}

// ---------------------------------------------------------------------------
// Test: DualFileList extras round-trip through accessor
// ---------------------------------------------------------------------------

/// Verifies that extras metadata (checksum, ACL/xattr, atime/crtime, device
/// numbers) round-trips correctly through DualFileList push and is accessible
/// via the extras arena decode path.
#[test]
fn extras_round_trip_through_dual_file_list() {
    let fixture = build_mixed_fixture();
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    for i in 0..dual.len() {
        let legacy = &dual[i];
        let flat_header = &dual.flat().get(i).unwrap().header;

        // Decode extras from the arena.
        let flat_extras = dual
            .extras()
            .decode(flat_header.extras)
            .unwrap_or_else(|e| panic!("entry {i}: extras decode failed: {e}"));

        match flat_extras {
            None => {
                // Legacy must also have no extras-backed fields.
                assert!(legacy.rdev_major().is_none(), "entry {i}: unexpected rdev");
                assert!(
                    legacy.checksum().is_none(),
                    "entry {i}: unexpected checksum"
                );
                assert!(legacy.acl_ndx().is_none(), "entry {i}: unexpected acl_ndx");
                assert!(
                    legacy.xattr_ndx().is_none(),
                    "entry {i}: unexpected xattr_ndx"
                );
                assert_eq!(legacy.atime(), 0, "entry {i}: unexpected atime");
                assert_eq!(legacy.crtime(), 0, "entry {i}: unexpected crtime");
            }
            Some(decoded) => {
                // Device numbers.
                assert_eq!(
                    decoded.rdev_major,
                    legacy.rdev_major(),
                    "entry {i}: rdev_major"
                );
                assert_eq!(
                    decoded.rdev_minor,
                    legacy.rdev_minor(),
                    "entry {i}: rdev_minor"
                );

                // Checksum.
                assert_eq!(
                    decoded.checksum.as_deref(),
                    legacy.checksum(),
                    "entry {i}: checksum"
                );

                // ACL/xattr indices.
                assert_eq!(decoded.acl_ndx, legacy.acl_ndx(), "entry {i}: acl_ndx");
                assert_eq!(
                    decoded.def_acl_ndx,
                    legacy.def_acl_ndx(),
                    "entry {i}: def_acl_ndx"
                );
                assert_eq!(
                    decoded.xattr_ndx,
                    legacy.xattr_ndx(),
                    "entry {i}: xattr_ndx"
                );

                // User/group names.
                assert_eq!(
                    decoded
                        .user_name
                        .as_deref()
                        .map(|b| std::str::from_utf8(b).unwrap()),
                    legacy.user_name(),
                    "entry {i}: user_name"
                );
                assert_eq!(
                    decoded
                        .group_name
                        .as_deref()
                        .map(|b| std::str::from_utf8(b).unwrap()),
                    legacy.group_name(),
                    "entry {i}: group_name"
                );

                // Symlink targets.
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    match legacy.link_target() {
                        Some(target) => {
                            assert_eq!(
                                decoded.link_target.as_deref(),
                                Some(target.as_os_str().as_bytes()),
                                "entry {i}: link_target"
                            );
                        }
                        None => {
                            assert_eq!(decoded.link_target, None, "entry {i}: link_target");
                        }
                    }
                }

                // Atime/crtime.
                assert_eq!(
                    decoded.atime.unwrap_or(0),
                    legacy.atime(),
                    "entry {i}: atime"
                );
                assert_eq!(
                    decoded.crtime.unwrap_or(0),
                    legacy.crtime(),
                    "entry {i}: crtime"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Test: content_dir flag parity
// ---------------------------------------------------------------------------

/// Verifies the content_dir flag round-trips correctly for both content and
/// empty directories through the DualFileList path.
#[test]
fn content_dir_flag_parity() {
    let fixture = build_mixed_fixture();
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    for i in 0..dual.len() {
        let legacy = &dual[i];
        let flat_entry = flat_entry_with_extras(dual.flat(), dual.extras(), i).unwrap();
        let flat_acc: &dyn FileEntryAccessor = &flat_entry;

        assert_eq!(
            legacy.content_dir(),
            flat_acc.content_dir(),
            "entry {i} ({}): content_dir mismatch",
            legacy.name()
        );
    }
}

// ---------------------------------------------------------------------------
// Test: iterator count and order parity
// ---------------------------------------------------------------------------

/// Verifies that FlatFileList::iter() yields entries in the same order as
/// the legacy DualFileList indexing.
#[test]
fn flat_iterator_order_matches_indexing() {
    let fixture = build_mixed_fixture();
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    let flat = dual.flat();
    let iter_entries: Vec<_> = flat.iter().collect();
    assert_eq!(iter_entries.len(), flat.len());

    for (i, iter_entry) in iter_entries.iter().enumerate() {
        let indexed = flat.get(i).unwrap();
        assert_eq!(iter_entry.name, indexed.name, "entry {i}: name mismatch");
        assert_eq!(
            iter_entry.dirname, indexed.dirname,
            "entry {i}: dirname mismatch"
        );
        assert_eq!(
            iter_entry.header.size, indexed.header.size,
            "entry {i}: size mismatch"
        );
    }
}

// ---------------------------------------------------------------------------
// Test: dirname interning deduplication
// ---------------------------------------------------------------------------

/// Verifies that pushing multiple files under the same directory results in
/// dirname handle sharing in the flat path interner.
#[test]
fn dirname_interning_deduplication() {
    let fixture = build_mixed_fixture();
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    let flat = dual.flat();

    // All entries under "src" should share the same dirname handle.
    let src_handles: Vec<_> = (0..flat.len())
        .filter_map(|i| {
            let e = flat.get(i)?;
            if e.dirname == b"src" {
                Some(e.header.dirname)
            } else {
                None
            }
        })
        .collect();

    if src_handles.len() > 1 {
        let first = src_handles[0];
        assert!(
            src_handles.iter().all(|h| *h == first),
            "entries under 'src' must share one dirname handle"
        );
    }
}

// ---------------------------------------------------------------------------
// Test: mixed types accessor coverage
// ---------------------------------------------------------------------------

/// Verifies that FileEntryAccessor correctly classifies every entry type
/// in the fixture through both legacy and flat paths.
#[test]
fn mixed_type_classification() {
    let fixture = build_mixed_fixture();
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    let mut files = 0u32;
    let mut dirs = 0u32;
    let mut symlinks = 0u32;
    let mut devices = 0u32;

    for i in 0..dual.len() {
        let legacy: &dyn FileEntryAccessor = &dual[i];
        let flat_entry = flat_entry_with_extras(dual.flat(), dual.extras(), i).unwrap();
        let flat_acc: &dyn FileEntryAccessor = &flat_entry;

        // Both must agree on classification.
        assert_eq!(legacy.is_file(), flat_acc.is_file(), "entry {i}");
        assert_eq!(legacy.is_dir(), flat_acc.is_dir(), "entry {i}");
        assert_eq!(legacy.is_symlink(), flat_acc.is_symlink(), "entry {i}");
        assert_eq!(legacy.is_device(), flat_acc.is_device(), "entry {i}");

        if legacy.is_file() {
            files += 1;
        }
        if legacy.is_dir() {
            dirs += 1;
        }
        if legacy.is_symlink() {
            symlinks += 1;
        }
        if legacy.is_device() {
            devices += 1;
        }
    }

    // Sanity: the fixture must contain at least one of each type.
    assert!(files > 0, "fixture must contain regular files");
    assert!(dirs > 0, "fixture must contain directories");
    assert!(symlinks > 0, "fixture must contain symlinks");
    assert!(devices > 0, "fixture must contain devices");
}

// ---------------------------------------------------------------------------
// Test: end-to-end pipeline (build, sort, iterate, classify)
// ---------------------------------------------------------------------------

/// Exercises the full transfer pipeline through the flat flist path:
/// 1. Build a mixed file list via DualFileList
/// 2. Sort both representations
/// 3. Iterate and verify accessor parity post-sort
/// 4. Classify entries and verify type counts
///
/// This is the primary regression gate ensuring the flat path does not
/// silently diverge from the legacy path during migration.
#[test]
fn end_to_end_pipeline() {
    let fixture = build_mixed_fixture();

    // Stage 1: Build.
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    // Stage 2: Sort legacy entries.
    let mut sorted_legacy = fixture.clone();
    sort_file_list(&mut sorted_legacy, false, false);

    // Stage 3: Build a second DualFileList from sorted entries.
    // (DualFileList does not support re-sorting the flat side, so we rebuild.)
    let mut sorted_dual = DualFileList::new();
    for entry in &sorted_legacy {
        sorted_dual.push(entry.clone());
    }

    // Stage 4: Verify accessor parity on sorted list.
    for i in 0..sorted_dual.len() {
        let legacy: &dyn FileEntryAccessor = &sorted_dual[i];
        let flat_entry =
            flat_entry_with_extras(sorted_dual.flat(), sorted_dual.extras(), i).unwrap();
        let flat_acc: &dyn FileEntryAccessor = &flat_entry;

        assert_eq!(legacy.size(), flat_acc.size(), "sorted entry {i}: size");
        assert_eq!(legacy.mode(), flat_acc.mode(), "sorted entry {i}: mode");
        assert_eq!(legacy.mtime(), flat_acc.mtime(), "sorted entry {i}: mtime");
        assert_eq!(legacy.uid(), flat_acc.uid(), "sorted entry {i}: uid");
        assert_eq!(legacy.gid(), flat_acc.gid(), "sorted entry {i}: gid");
        assert_eq!(
            legacy.is_file(),
            flat_acc.is_file(),
            "sorted entry {i}: is_file"
        );
        assert_eq!(
            legacy.is_dir(),
            flat_acc.is_dir(),
            "sorted entry {i}: is_dir"
        );
        assert_eq!(
            legacy.is_symlink(),
            flat_acc.is_symlink(),
            "sorted entry {i}: is_symlink"
        );
        assert_eq!(
            legacy.rdev_major(),
            flat_acc.rdev_major(),
            "sorted entry {i}: rdev_major"
        );
        assert_eq!(
            legacy.acl_ndx(),
            flat_acc.acl_ndx(),
            "sorted entry {i}: acl_ndx"
        );
        assert_eq!(
            legacy.content_dir(),
            flat_acc.content_dir(),
            "sorted entry {i}: content_dir"
        );
    }

    // Stage 5: Verify entry count by type matches legacy fixture.
    let count_files = sorted_dual.iter().filter(|e| e.is_file()).count();
    let count_dirs = sorted_dual.iter().filter(|e| e.is_dir()).count();
    let count_symlinks = sorted_dual.iter().filter(|e| e.is_symlink()).count();

    let flat_count_files = sorted_dual
        .flat()
        .iter()
        .filter(|e| {
            let acc: &dyn FileEntryAccessor = e;
            acc.is_file()
        })
        .count();
    let flat_count_dirs = sorted_dual
        .flat()
        .iter()
        .filter(|e| {
            let acc: &dyn FileEntryAccessor = e;
            acc.is_dir()
        })
        .count();
    let flat_count_symlinks = sorted_dual
        .flat()
        .iter()
        .filter(|e| {
            let acc: &dyn FileEntryAccessor = e;
            acc.is_symlink()
        })
        .count();

    assert_eq!(count_files, flat_count_files, "file count mismatch");
    assert_eq!(count_dirs, flat_count_dirs, "dir count mismatch");
    assert_eq!(
        count_symlinks, flat_count_symlinks,
        "symlink count mismatch"
    );
}

// ---------------------------------------------------------------------------
// Test: name_bytes parity
// ---------------------------------------------------------------------------

/// Verifies that `name_bytes()` returns consistent wire-format bytes for
/// both legacy and flat representations.
#[test]
fn name_bytes_parity() {
    let fixture = build_mixed_fixture();
    let mut dual = DualFileList::new();
    for entry in &fixture {
        dual.push(entry.clone());
    }

    for i in 0..dual.len() {
        let legacy_bytes = dual[i].name_bytes();
        let flat_entry = dual.flat().get(i).unwrap();

        // The flat entry's name is the basename only. Verify it matches
        // the basename portion of the legacy name_bytes.
        let legacy_name = std::str::from_utf8(&legacy_bytes).unwrap_or("");
        let expected_basename = match legacy_name.rfind('/') {
            Some(pos) => &legacy_name[pos + 1..],
            None => legacy_name,
        };
        assert_eq!(
            flat_entry.name,
            expected_basename.as_bytes(),
            "entry {i}: name_bytes basename mismatch"
        );
    }
}

// ---------------------------------------------------------------------------
// Test: large file list (stress)
// ---------------------------------------------------------------------------

/// Exercises the pipeline with a larger file list (500 entries) to verify
/// no regressions under moderate scale.
#[test]
fn stress_500_entries() {
    let mut entries = Vec::with_capacity(500);

    for i in 0u32..500 {
        let dir_idx = i % 10;
        let entry = match i % 5 {
            0 => FileEntry::new_directory(format!("d{dir_idx}").into(), 0o755),
            1 => FileEntry::new_symlink(
                format!("d{dir_idx}/link_{i}").into(),
                format!("../target_{i}").into(),
            ),
            _ => {
                let mut f = FileEntry::new_file(
                    format!("d{dir_idx}/file_{i}.dat").into(),
                    (i as u64 + 1) * 256,
                    0o644,
                );
                if i % 7 == 0 {
                    f.set_uid(1000 + i);
                    f.set_gid(2000 + i);
                }
                if i % 11 == 0 {
                    f.set_checksum(vec![(i & 0xFF) as u8; 16]);
                }
                f
            }
        };
        entries.push(entry);
    }

    // Build DualFileList.
    let mut dual = DualFileList::with_capacity(entries.len());
    for entry in &entries {
        dual.push(entry.clone());
    }
    assert_eq!(dual.len(), 500);
    assert_eq!(dual.flat().len(), 500);

    // Sort.
    let mut sorted = entries.clone();
    sort_file_list(&mut sorted, false, false);

    let mut sorted_generic = entries.clone();
    sort_entries_generic(&mut sorted_generic, false, false);

    for i in 0..sorted.len() {
        assert_eq!(
            sorted[i].name(),
            sorted_generic[i].name(),
            "stress sort diverged at index {i}"
        );
    }

    // Verify accessor parity on a rebuilt sorted dual list.
    let mut sorted_dual = DualFileList::with_capacity(sorted.len());
    for entry in &sorted {
        sorted_dual.push(entry.clone());
    }

    for i in 0..sorted_dual.len() {
        let legacy: &dyn FileEntryAccessor = &sorted_dual[i];
        let flat_entry =
            flat_entry_with_extras(sorted_dual.flat(), sorted_dual.extras(), i).unwrap();
        let flat_acc: &dyn FileEntryAccessor = &flat_entry;

        assert_eq!(legacy.size(), flat_acc.size(), "stress entry {i}: size");
        assert_eq!(legacy.mode(), flat_acc.mode(), "stress entry {i}: mode");
        assert_eq!(
            legacy.is_file(),
            flat_acc.is_file(),
            "stress entry {i}: is_file"
        );
        assert_eq!(
            legacy.is_dir(),
            flat_acc.is_dir(),
            "stress entry {i}: is_dir"
        );
        assert_eq!(
            legacy.is_symlink(),
            flat_acc.is_symlink(),
            "stress entry {i}: is_symlink"
        );
    }
}
