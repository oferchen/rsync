use super::*;

/// A blank header used as a starting point; every optional field is absent.
fn empty_header() -> FileEntryHeader {
    FileEntryHeader {
        mtime: 0,
        size: 0,
        uid: 0,
        gid: 0,
        name: PathHandle::NONE,
        dirname: PathHandle::NONE,
        extras: ExtrasRef::NO_EXTRAS,
        mtime_nsec: 0,
        mode: 0,
        flags: 0,
        present: 0,
    }
}

#[test]
fn header_fits_size_target() {
    // The design targets 48-64 bytes; the chosen field order yields 48.
    assert!(core::mem::size_of::<FileEntryHeader>() <= 64);
}

#[test]
fn present_bit_get_set_round_trips() {
    let mut h = empty_header();
    assert!(!h.has(PRESENT_UID));
    assert!(!h.has(PRESENT_GID));

    h.set(PRESENT_UID);
    assert!(h.has(PRESENT_UID));
    // Setting one bit must not leak into the others.
    assert!(!h.has(PRESENT_GID));

    h.set(PRESENT_GID);
    assert!(h.has(PRESENT_GID));
    // set() is idempotent and additive.
    h.set(PRESENT_UID);
    assert!(h.has(PRESENT_UID));
    assert!(h.has(PRESENT_GID));
}

#[test]
fn all_present_bits_are_distinct() {
    let bits = [
        PRESENT_UID,
        PRESENT_GID,
        PRESENT_MTIME_NSEC,
        PRESENT_CONTENT_DIR,
        PRESENT_LENGTH64,
    ];
    let mut acc = 0u16;
    for bit in bits {
        // Each flag is a single, previously-unset bit.
        assert_eq!(bit.count_ones(), 1);
        assert_eq!(acc & bit, 0);
        acc |= bit;
    }
}

#[test]
fn uid_is_none_until_bit_set() {
    let mut h = empty_header();
    h.uid = 1000;
    // Value is stored but not yet visible without the presence bit.
    assert_eq!(h.uid(), None);
    h.set(PRESENT_UID);
    assert_eq!(h.uid(), Some(1000));
}

#[test]
fn gid_is_none_until_bit_set() {
    let mut h = empty_header();
    h.gid = 2000;
    assert_eq!(h.gid(), None);
    h.set(PRESENT_GID);
    assert_eq!(h.gid(), Some(2000));
}

#[test]
fn mtime_nsec_is_none_until_bit_set() {
    let mut h = empty_header();
    h.mtime_nsec = 123_456_789;
    assert_eq!(h.mtime_nsec(), None);
    h.set(PRESENT_MTIME_NSEC);
    assert_eq!(h.mtime_nsec(), Some(123_456_789));
}

#[test]
fn path_handle_none_sentinel() {
    assert_eq!(PathHandle::NONE, PathHandle(u32::MAX));
    let h = empty_header();
    assert_eq!(h.name, PathHandle::NONE);
    assert_eq!(h.dirname, PathHandle::NONE);
    // A real handle is distinguishable from the sentinel.
    assert_ne!(PathHandle(0), PathHandle::NONE);
}

#[test]
fn extras_ref_no_extras_sentinel() {
    assert_eq!(ExtrasRef::NO_EXTRAS, ExtrasRef(u32::MAX));
    let h = empty_header();
    assert_eq!(h.extras, ExtrasRef::NO_EXTRAS);
    assert_ne!(ExtrasRef(0), ExtrasRef::NO_EXTRAS);
}

#[test]
fn intern_resolve_round_trips() {
    let mut arena = PathArena::new();
    let h = arena.intern("src/lib.rs");
    // A real string never collides with the empty sentinel.
    assert_ne!(h, PathHandle::NONE);
    assert_eq!(arena.resolve(h), "src/lib.rs");
    assert_eq!(arena.get(h), Some("src/lib.rs"));
}

#[test]
fn intern_dedups_to_same_handle() {
    let mut arena = PathArena::new();
    let a = arena.intern("README");
    let b = arena.intern("README");
    // Dedup yields the same handle and stores the bytes only once.
    assert_eq!(a, b);
    assert_eq!(arena.len(), 1);
    assert_eq!(arena.bytes_len(), "README".len());
}

#[test]
fn distinct_strings_get_distinct_handles() {
    let mut arena = PathArena::new();
    let a = arena.intern("alpha");
    let b = arena.intern("beta");
    let c = arena.intern("gamma");
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
    assert_eq!(arena.len(), 3);
    // Each resolves back to its own string regardless of insertion order.
    assert_eq!(arena.resolve(a), "alpha");
    assert_eq!(arena.resolve(b), "beta");
    assert_eq!(arena.resolve(c), "gamma");
}

#[test]
fn empty_string_is_the_none_sentinel() {
    let mut arena = PathArena::new();
    // The empty name/dirname slot must map to NONE and store nothing.
    assert_eq!(arena.intern(""), PathHandle::NONE);
    assert!(arena.is_empty());
    assert_eq!(arena.bytes_len(), 0);
    // NONE resolves to the empty string and to None, never to stored bytes.
    assert_eq!(arena.resolve(PathHandle::NONE), "");
    assert_eq!(arena.get(PathHandle::NONE), None);
}

#[test]
fn get_returns_none_for_unknown_handle() {
    let arena = PathArena::new();
    // An index never issued by this arena is not a valid handle.
    assert_eq!(arena.get(PathHandle(0)), None);
}

#[test]
fn dedup_shares_basenames_across_dirnames() {
    // Mirrors the design's basename-dedup goal: two identical basenames
    // under different dirnames collapse to one arena string, which upstream
    // (inline flexible-array basenames) cannot do.
    let mut arena = PathArena::new();
    let dir_a = arena.intern("a");
    let dir_b = arena.intern("b");
    let name_1 = arena.intern("README");
    let name_2 = arena.intern("README");
    assert_ne!(dir_a, dir_b);
    assert_eq!(name_1, name_2);
    // Two dirnames + one shared basename = three unique strings.
    assert_eq!(arena.len(), 3);
}

#[test]
fn with_capacity_starts_empty() {
    let arena = PathArena::with_capacity(128);
    assert!(arena.is_empty());
    assert_eq!(arena.len(), 0);
}

// ---------------------------------------------------------------------------
// FlatFileList tests
// ---------------------------------------------------------------------------

/// Helper: push an entry with the given name and dirname into `flist`.
fn push_entry(flist: &mut FlatFileList, name: &str, dirname: &str, size: u64) {
    let name_h = flist.paths_mut().intern(name);
    let dirname_h = flist.paths_mut().intern(dirname);
    let mut h = empty_header();
    h.name = name_h;
    h.dirname = dirname_h;
    h.size = size;
    flist.push(h);
}

#[test]
fn flat_file_list_new_is_empty() {
    let flist = FlatFileList::new();
    assert!(flist.is_empty());
    assert_eq!(flist.len(), 0);
    assert!(flist.get(0).is_none());
}

#[test]
fn flat_file_list_with_capacity_is_empty() {
    let flist = FlatFileList::with_capacity(64);
    assert!(flist.is_empty());
    assert_eq!(flist.len(), 0);
}

#[test]
fn flat_file_list_push_and_get() {
    let mut flist = FlatFileList::new();
    push_entry(&mut flist, "README", "src", 42);

    assert_eq!(flist.len(), 1);
    assert!(!flist.is_empty());

    let entry = flist.get(0).expect("entry 0 should exist");
    assert_eq!(entry.name, b"README");
    assert_eq!(entry.dirname, b"src");
    assert_eq!(entry.header.size, 42);
}

#[test]
fn flat_file_list_get_out_of_bounds() {
    let mut flist = FlatFileList::new();
    push_entry(&mut flist, "a", "", 0);
    assert!(flist.get(1).is_none());
    assert!(flist.get(100).is_none());
}

#[test]
fn flat_file_list_multiple_entries() {
    let mut flist = FlatFileList::new();
    push_entry(&mut flist, "alpha", "d1", 10);
    push_entry(&mut flist, "beta", "d2", 20);
    push_entry(&mut flist, "gamma", "d1", 30);

    assert_eq!(flist.len(), 3);

    let e0 = flist.get(0).unwrap();
    assert_eq!(e0.name, b"alpha");
    assert_eq!(e0.dirname, b"d1");

    let e1 = flist.get(1).unwrap();
    assert_eq!(e1.name, b"beta");
    assert_eq!(e1.dirname, b"d2");

    let e2 = flist.get(2).unwrap();
    assert_eq!(e2.name, b"gamma");
    assert_eq!(e2.dirname, b"d1");
}

#[test]
fn flat_file_list_iter_count_and_order() {
    let mut flist = FlatFileList::new();
    let names = ["one", "two", "three", "four"];
    for name in &names {
        push_entry(&mut flist, name, "", 0);
    }

    let collected: Vec<&[u8]> = flist.iter().map(|e| e.name).collect();
    assert_eq!(collected.len(), names.len());
    for (got, expected) in collected.iter().zip(names.iter()) {
        assert_eq!(*got, expected.as_bytes());
    }
}

#[test]
fn flat_file_list_iter_empty() {
    let flist = FlatFileList::new();
    assert_eq!(flist.iter().count(), 0);
}

#[test]
fn flat_file_list_sort_by_name() {
    let mut flist = FlatFileList::new();
    // Push in reverse alphabetical order, same dirname.
    push_entry(&mut flist, "cherry", "", 3);
    push_entry(&mut flist, "apple", "", 1);
    push_entry(&mut flist, "banana", "", 2);

    flist.sort();

    let sorted: Vec<&[u8]> = flist.iter().map(|e| e.name).collect();
    assert_eq!(sorted, vec![b"apple" as &[u8], b"banana", b"cherry"]);

    // Verify sizes followed their headers through the sort.
    assert_eq!(flist.get(0).unwrap().header.size, 1);
    assert_eq!(flist.get(1).unwrap().header.size, 2);
    assert_eq!(flist.get(2).unwrap().header.size, 3);
}

#[test]
fn flat_file_list_sort_by_dirname_then_name() {
    let mut flist = FlatFileList::new();
    push_entry(&mut flist, "z", "b", 1);
    push_entry(&mut flist, "a", "b", 2);
    push_entry(&mut flist, "m", "a", 3);

    flist.sort();

    let sorted: Vec<(&[u8], &[u8])> = flist.iter().map(|e| (e.dirname, e.name)).collect();
    // "a/m" < "b/a" < "b/z"
    assert_eq!(
        sorted,
        vec![
            (b"a" as &[u8], b"m" as &[u8]),
            (b"b" as &[u8], b"a" as &[u8]),
            (b"b" as &[u8], b"z" as &[u8]),
        ]
    );
}

#[test]
fn flat_file_list_paths_accessor() {
    let mut flist = FlatFileList::new();
    push_entry(&mut flist, "file.txt", "dir", 0);

    // Shared accessor can resolve handles.
    assert_eq!(flist.paths().len(), 2); // "file.txt" and "dir"
    assert!(!flist.paths().is_empty());
}

#[test]
fn flat_file_list_deduped_names_share_handles() {
    let mut flist = FlatFileList::new();
    // Two entries with the same basename under different dirnames.
    push_entry(&mut flist, "README", "src", 10);
    push_entry(&mut flist, "README", "docs", 20);

    // The interner deduplicates "README" - only 3 unique strings.
    assert_eq!(flist.paths().len(), 3); // "README", "src", "docs"

    let e0 = flist.get(0).unwrap();
    let e1 = flist.get(1).unwrap();
    assert_eq!(e0.header.name, e1.header.name); // Same handle
    assert_eq!(e0.name, e1.name); // Same resolved bytes
    assert_ne!(e0.header.dirname, e1.header.dirname); // Different dirnames
}

#[test]
fn flat_file_list_default_is_new() {
    let flist = FlatFileList::default();
    assert!(flist.is_empty());
    assert_eq!(flist.len(), 0);
}

// ---------------------------------------------------------------------------
// FlatFileList extras wiring (RSS-A.6.f)
// ---------------------------------------------------------------------------

/// Helper: push an entry with extras into `flist`.
fn push_entry_with_extras(
    flist: &mut FlatFileList,
    name: &str,
    dirname: &str,
    size: u64,
    extras: &FlatExtras,
) {
    let name_h = flist.paths_mut().intern(name);
    let dirname_h = flist.paths_mut().intern(dirname);
    let mut h = empty_header();
    h.name = name_h;
    h.dirname = dirname_h;
    h.size = size;
    flist.push_with_extras(h, extras);
}

#[test]
fn push_with_extras_no_extras_yields_sentinel() {
    let mut flist = FlatFileList::new();
    push_entry_with_extras(&mut flist, "plain.txt", "", 100, &FlatExtras::default());

    let h = &flist.get(0).unwrap().header;
    assert_eq!(h.extras, ExtrasRef::NO_EXTRAS);
    assert!(flist.extras().is_empty());
    assert_eq!(flist.extras().decode(h.extras).unwrap(), None);
}

#[test]
fn push_with_extras_symlink_round_trip() {
    let mut flist = FlatFileList::new();
    let extras = FlatExtras {
        link_target: Some(b"../other/target".to_vec()),
        ..FlatExtras::default()
    };
    push_entry_with_extras(&mut flist, "link", "src", 0, &extras);

    let h = &flist.get(0).unwrap().header;
    assert_ne!(h.extras, ExtrasRef::NO_EXTRAS);
    let decoded = flist.extras().decode(h.extras).unwrap().unwrap();
    assert_eq!(
        decoded.link_target.as_deref(),
        Some(b"../other/target" as &[u8])
    );
    assert_eq!(decoded.rdev_major, None);
}

#[test]
fn push_with_extras_device_round_trip() {
    let mut flist = FlatFileList::new();
    let extras = FlatExtras {
        rdev_major: Some(8),
        rdev_minor: Some(17),
        ..FlatExtras::default()
    };
    push_entry_with_extras(&mut flist, "sda", "dev", 0, &extras);

    let decoded = flist
        .extras()
        .decode(flist.get(0).unwrap().header.extras)
        .unwrap()
        .unwrap();
    assert_eq!(decoded.rdev_major, Some(8));
    assert_eq!(decoded.rdev_minor, Some(17));
}

#[test]
fn push_with_extras_all_fields_round_trip() {
    let mut flist = FlatFileList::new();
    let extras = FlatExtras {
        link_target: Some(b"target".to_vec()),
        rdev_major: Some(1),
        rdev_minor: Some(2),
        hardlink_idx: Some(42),
        acl_ndx: Some(3),
        def_acl_ndx: Some(4),
        xattr_ndx: Some(5),
        checksum: Some(vec![0xDE; 16]),
        user_name: Some(b"alice".to_vec()),
        group_name: Some(b"staff".to_vec()),
        atime: Some(-12345),
        crtime: Some(987_654_321),
        atime_nsec: Some(500),
    };
    push_entry_with_extras(&mut flist, "f.txt", "dir", 1024, &extras);

    let decoded = flist
        .extras()
        .decode(flist.get(0).unwrap().header.extras)
        .unwrap()
        .unwrap();
    assert_eq!(decoded, extras);
}

#[test]
fn push_with_extras_multiple_entries_independent() {
    let mut flist = FlatFileList::new();

    let symlink_extras = FlatExtras {
        link_target: Some(b"../target".to_vec()),
        ..FlatExtras::default()
    };
    push_entry_with_extras(&mut flist, "link1", "", 0, &symlink_extras);

    push_entry_with_extras(&mut flist, "plain.txt", "", 256, &FlatExtras::default());

    let dev_extras = FlatExtras {
        rdev_major: Some(10),
        rdev_minor: Some(20),
        ..FlatExtras::default()
    };
    push_entry_with_extras(&mut flist, "sda", "dev", 0, &dev_extras);

    let checksum_extras = FlatExtras {
        checksum: Some(vec![0xAB; 8]),
        user_name: Some(b"bob".to_vec()),
        ..FlatExtras::default()
    };
    push_entry_with_extras(&mut flist, "data.bin", "out", 4096, &checksum_extras);

    assert_eq!(flist.len(), 4);

    let d0 = flist
        .extras()
        .decode(flist.get(0).unwrap().header.extras)
        .unwrap()
        .unwrap();
    assert_eq!(d0, symlink_extras);

    assert_eq!(flist.get(1).unwrap().header.extras, ExtrasRef::NO_EXTRAS);

    let d2 = flist
        .extras()
        .decode(flist.get(2).unwrap().header.extras)
        .unwrap()
        .unwrap();
    assert_eq!(d2, dev_extras);

    let d3 = flist
        .extras()
        .decode(flist.get(3).unwrap().header.extras)
        .unwrap()
        .unwrap();
    assert_eq!(d3, checksum_extras);
}

#[test]
fn push_with_extras_preserves_scalar_header_fields() {
    let mut flist = FlatFileList::new();
    let extras = FlatExtras {
        atime: Some(999_999),
        crtime: Some(888_888),
        ..FlatExtras::default()
    };
    let name_h = flist.paths_mut().intern("f.txt");
    let dirname_h = flist.paths_mut().intern("dir");
    let mut h = empty_header();
    h.name = name_h;
    h.dirname = dirname_h;
    h.size = 42;
    h.mtime = 1_000_000;
    h.mode = 0o100644;
    h.uid = 1000;
    h.gid = 2000;
    h.set(PRESENT_UID);
    h.set(PRESENT_GID);
    flist.push_with_extras(h, &extras);

    let entry = flist.get(0).unwrap();
    assert_eq!(entry.header.size, 42);
    assert_eq!(entry.header.mtime, 1_000_000);
    assert_eq!(entry.header.mode, 0o100644);
    assert_eq!(entry.header.uid(), Some(1000));
    assert_eq!(entry.header.gid(), Some(2000));
    assert_eq!(entry.name, b"f.txt");
    assert_eq!(entry.dirname, b"dir");

    let decoded = flist.extras().decode(entry.header.extras).unwrap().unwrap();
    assert_eq!(decoded.atime, Some(999_999));
    assert_eq!(decoded.crtime, Some(888_888));
}

#[test]
fn extras_accessor_starts_empty() {
    let flist = FlatFileList::new();
    assert!(flist.extras().is_empty());
    assert_eq!(flist.extras().len(), 0);
}

#[test]
fn extras_mut_allows_manual_append() {
    let mut flist = FlatFileList::new();
    let extras = FlatExtras {
        hardlink_idx: Some(77),
        ..FlatExtras::default()
    };
    let ext_ref = flist.extras_mut().append(&extras);

    let name_h = flist.paths_mut().intern("f.txt");
    let dirname_h = flist.paths_mut().intern("");
    let mut h = empty_header();
    h.name = name_h;
    h.dirname = dirname_h;
    h.extras = ext_ref;
    flist.push(h);

    let decoded = flist
        .extras()
        .decode(flist.get(0).unwrap().header.extras)
        .unwrap()
        .unwrap();
    assert_eq!(decoded.hardlink_idx, Some(77));
}

// ---------------------------------------------------------------------------
// Feature-flag coexistence (RSS-A.5.e.3)
// ---------------------------------------------------------------------------

/// Verifies PathArena dirname deduplication at scale: 100 files across
/// 5 directories stores each dirname exactly once, mirroring upstream
/// rsync's `lastdir` cache (upstream: flist.c:765-773).
#[test]
fn dirname_interning_deduplicates_at_scale() {
    let mut flist = FlatFileList::new();

    let dirs = ["alpha", "beta", "gamma", "delta", "epsilon"];
    for dir in &dirs {
        for i in 0u64..20 {
            let name = format!("f{i}.rs");
            push_entry(&mut flist, &name, dir, i);
        }
    }

    assert_eq!(flist.len(), 100);

    // PathArena should hold 5 unique dirnames + 20 unique basenames = 25.
    // Basenames "f0.rs"..."f19.rs" repeat across dirs but intern once each.
    assert_eq!(flist.paths().len(), 25);

    // Verify handle sharing: all 20 entries under "alpha" share one handle.
    let alpha_handles: Vec<_> = (0..flist.len())
        .filter_map(|i| {
            let e = flist.get(i)?;
            if e.dirname == b"alpha" {
                Some(e.header.dirname)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(alpha_handles.len(), 20);
    let first = alpha_handles[0];
    assert!(
        alpha_handles.iter().all(|h| *h == first),
        "all entries in 'alpha' must share one dirname handle"
    );
}

/// Verifies that the flat-flist types and the legacy `Vec<FileEntry>` path
/// compile and operate side-by-side in the same scope. This is the key
/// invariant of the feature flag: enabling `flat-flist` must never shadow,
/// conflict with, or break the legacy representation.
#[test]
fn flat_and_legacy_coexist_in_same_scope() {
    use crate::flist::FileEntry;

    // Legacy path: build a Vec<FileEntry> with two entries.
    let legacy: Vec<FileEntry> = vec![
        FileEntry::new_file("src/main.rs".into(), 2048, 0o644),
        FileEntry::new_file("README".into(), 512, 0o644),
    ];
    assert_eq!(legacy.len(), 2);
    assert_eq!(legacy[0].size(), 2048);

    // Flat path: build a FlatFileList with the same logical entries.
    let mut flat = FlatFileList::new();
    push_entry(&mut flat, "main.rs", "src", 2048);
    push_entry(&mut flat, "README", "", 512);
    assert_eq!(flat.len(), 2);
    assert_eq!(flat.get(0).unwrap().header.size, 2048);

    // Both representations live in the same scope without name collisions
    // or trait ambiguity. The legacy path is completely unaffected by the
    // feature flag.
    assert_eq!(legacy.len(), flat.len());
}

// ---------------------------------------------------------------------------
// INC_RECURSE segment-append tests (RSS-A.8.c)
// ---------------------------------------------------------------------------

/// Helper: intern paths and build a header for a (name, dirname, size) triple.
fn make_header(flist: &mut FlatFileList, name: &str, dirname: &str, size: u64) -> FileEntryHeader {
    let name_h = flist.paths_mut().intern(name);
    let dirname_h = flist.paths_mut().intern(dirname);
    let mut h = empty_header();
    h.name = name_h;
    h.dirname = dirname_h;
    h.size = size;
    h
}

/// Helper: intern paths, build header with extras, encode extras into the
/// arena, and return the ready-to-push header.
fn make_header_with_extras(
    flist: &mut FlatFileList,
    name: &str,
    dirname: &str,
    size: u64,
    extras: &FlatExtras,
) -> FileEntryHeader {
    let name_h = flist.paths_mut().intern(name);
    let dirname_h = flist.paths_mut().intern(dirname);
    let mut h = empty_header();
    h.name = name_h;
    h.dirname = dirname_h;
    h.size = size;
    h.extras = flist.extras_mut().append(extras);
    h
}

#[test]
fn segment_single_append_and_handles_valid() {
    let mut flist = FlatFileList::new();

    // Build headers for one segment.
    let h0 = make_header(&mut flist, "alpha.rs", "src", 100);
    let h1 = make_header(&mut flist, "beta.rs", "src", 200);
    let h2 = make_header(&mut flist, "gamma.rs", "tests", 300);

    let seg_idx = flist.extend_segment(&[h0, h1, h2]);

    assert_eq!(seg_idx, 0);
    assert_eq!(flist.segment_count(), 1);
    assert_eq!(flist.len(), 3);

    let seg = flist.segment(0).unwrap();
    assert_eq!(seg.start_index, 0);
    assert_eq!(seg.count, 3);

    let range = flist.segment_range(0).unwrap();
    assert_eq!(range, 0..3);

    // Verify all handles resolve correctly.
    let e0 = flist.get(0).unwrap();
    assert_eq!(e0.name, b"alpha.rs");
    assert_eq!(e0.dirname, b"src");
    assert_eq!(e0.header.size, 100);

    let e1 = flist.get(1).unwrap();
    assert_eq!(e1.name, b"beta.rs");
    assert_eq!(e1.dirname, b"src");
    assert_eq!(e1.header.size, 200);

    let e2 = flist.get(2).unwrap();
    assert_eq!(e2.name, b"gamma.rs");
    assert_eq!(e2.dirname, b"tests");
    assert_eq!(e2.header.size, 300);
}

#[test]
fn segment_multi_append_all_handles_valid() {
    let mut flist = FlatFileList::new();

    // Segment 0: root-level files.
    let h0 = make_header(&mut flist, "README", "", 50);
    let h1 = make_header(&mut flist, "Cargo.toml", "", 80);
    let s0 = flist.extend_segment(&[h0, h1]);
    assert_eq!(s0, 0);

    // Record handles from segment 0 for later validation.
    let seg0_name0 = flist.get(0).unwrap().header.name;
    let seg0_name1 = flist.get(1).unwrap().header.name;

    // Segment 1: src/ files.
    let h2 = make_header(&mut flist, "main.rs", "src", 1024);
    let h3 = make_header(&mut flist, "lib.rs", "src", 512);
    let h4 = make_header(&mut flist, "util.rs", "src", 256);
    let s1 = flist.extend_segment(&[h2, h3, h4]);
    assert_eq!(s1, 1);

    // Segment 2: tests/ files.
    let h5 = make_header(&mut flist, "integration.rs", "tests", 2048);
    let s2 = flist.extend_segment(&[h5]);
    assert_eq!(s2, 2);

    // Segment 3: deeply nested.
    let h6 = make_header(&mut flist, "mod.rs", "src/engine/delta", 768);
    let h7 = make_header(&mut flist, "apply.rs", "src/engine/delta", 640);
    let s3 = flist.extend_segment(&[h6, h7]);
    assert_eq!(s3, 3);

    // Verify segment metadata.
    assert_eq!(flist.segment_count(), 4);
    assert_eq!(flist.len(), 8);
    assert_eq!(flist.segments().len(), 4);

    assert_eq!(flist.segment_range(0), Some(0..2));
    assert_eq!(flist.segment_range(1), Some(2..5));
    assert_eq!(flist.segment_range(2), Some(5..6));
    assert_eq!(flist.segment_range(3), Some(6..8));

    // Verify that segment-0 handles still resolve after three more segments
    // were appended (potential Vec reallocation).
    assert_eq!(flist.get(0).unwrap().header.name, seg0_name0);
    assert_eq!(flist.get(1).unwrap().header.name, seg0_name1);
    assert_eq!(flist.paths().resolve(seg0_name0), "README");
    assert_eq!(flist.paths().resolve(seg0_name1), "Cargo.toml");

    // Verify every entry resolves correctly.
    let expected: &[(&[u8], &[u8], u64)] = &[
        (b"README", b"", 50),
        (b"Cargo.toml", b"", 80),
        (b"main.rs", b"src", 1024),
        (b"lib.rs", b"src", 512),
        (b"util.rs", b"src", 256),
        (b"integration.rs", b"tests", 2048),
        (b"mod.rs", b"src/engine/delta", 768),
        (b"apply.rs", b"src/engine/delta", 640),
    ];
    for (i, &(name, dirname, size)) in expected.iter().enumerate() {
        let e = flist.get(i).unwrap();
        assert_eq!(e.name, name, "entry {i} name mismatch");
        assert_eq!(e.dirname, dirname, "entry {i} dirname mismatch");
        assert_eq!(e.header.size, size, "entry {i} size mismatch");
    }
}

#[test]
fn segment_multi_append_with_extras_handles_valid() {
    let mut flist = FlatFileList::new();

    // Segment 0: plain files (no extras).
    let h0 = make_header(&mut flist, "a.txt", "dir", 10);
    flist.extend_segment(&[h0]);

    // Segment 1: entries with extras.
    let extras1 = FlatExtras {
        link_target: Some(b"../target".to_vec()),
        ..FlatExtras::default()
    };
    let h1 = make_header_with_extras(&mut flist, "link", "dir", 0, &extras1);

    let extras2 = FlatExtras {
        checksum: Some(vec![0xAB; 16]),
        user_name: Some(b"alice".to_vec()),
        ..FlatExtras::default()
    };
    let h2 = make_header_with_extras(&mut flist, "data.bin", "out", 4096, &extras2);
    flist.extend_segment(&[h1, h2]);

    // Segment 2: another plain file.
    let h3 = make_header(&mut flist, "z.txt", "dir", 20);
    flist.extend_segment(&[h3]);

    assert_eq!(flist.segment_count(), 3);
    assert_eq!(flist.len(), 4);

    // Verify segment-0 entry is still accessible.
    let e0 = flist.get(0).unwrap();
    assert_eq!(e0.name, b"a.txt");
    assert_eq!(e0.header.extras, ExtrasRef::NO_EXTRAS);

    // Verify segment-1 extras round-trip.
    let e1 = flist.get(1).unwrap();
    let d1 = flist.extras().decode(e1.header.extras).unwrap().unwrap();
    assert_eq!(d1.link_target.as_deref(), Some(b"../target" as &[u8]));

    let e2 = flist.get(2).unwrap();
    let d2 = flist.extras().decode(e2.header.extras).unwrap().unwrap();
    assert_eq!(d2.checksum, Some(vec![0xAB; 16]));
    assert_eq!(d2.user_name, Some(b"alice".to_vec()));

    // Verify segment-2 entry.
    let e3 = flist.get(3).unwrap();
    assert_eq!(e3.name, b"z.txt");
    assert_eq!(e3.header.extras, ExtrasRef::NO_EXTRAS);
}

#[test]
fn sort_segment_does_not_affect_other_segments() {
    let mut flist = FlatFileList::new();

    // Segment 0: intentionally reverse-sorted.
    let h0 = make_header(&mut flist, "cherry", "fruit", 3);
    let h1 = make_header(&mut flist, "banana", "fruit", 2);
    let h2 = make_header(&mut flist, "apple", "fruit", 1);
    flist.extend_segment(&[h0, h1, h2]);

    // Segment 1: also reverse-sorted.
    let h3 = make_header(&mut flist, "zeta", "greek", 30);
    let h4 = make_header(&mut flist, "eta", "greek", 20);
    let h5 = make_header(&mut flist, "alpha", "greek", 10);
    flist.extend_segment(&[h3, h4, h5]);

    // Segment 2: single entry (sort is a no-op).
    let h6 = make_header(&mut flist, "only.txt", "", 99);
    flist.extend_segment(&[h6]);

    // Sort only segment 1.
    flist.sort_segment(1);

    // Segment 0 must remain in original (reverse) order.
    assert_eq!(flist.get(0).unwrap().name, b"cherry");
    assert_eq!(flist.get(0).unwrap().header.size, 3);
    assert_eq!(flist.get(1).unwrap().name, b"banana");
    assert_eq!(flist.get(1).unwrap().header.size, 2);
    assert_eq!(flist.get(2).unwrap().name, b"apple");
    assert_eq!(flist.get(2).unwrap().header.size, 1);

    // Segment 1 must now be sorted (alpha, eta, zeta).
    assert_eq!(flist.get(3).unwrap().name, b"alpha");
    assert_eq!(flist.get(3).unwrap().header.size, 10);
    assert_eq!(flist.get(4).unwrap().name, b"eta");
    assert_eq!(flist.get(4).unwrap().header.size, 20);
    assert_eq!(flist.get(5).unwrap().name, b"zeta");
    assert_eq!(flist.get(5).unwrap().header.size, 30);

    // Segment 2 must be untouched.
    assert_eq!(flist.get(6).unwrap().name, b"only.txt");
    assert_eq!(flist.get(6).unwrap().header.size, 99);
}

#[test]
fn sort_segment_with_dirname_ordering() {
    let mut flist = FlatFileList::new();

    // Segment with mixed dirnames - sort should order by dirname first,
    // then by name within the same dirname.
    let h0 = make_header(&mut flist, "z.rs", "b", 1);
    let h1 = make_header(&mut flist, "a.rs", "b", 2);
    let h2 = make_header(&mut flist, "m.rs", "a", 3);
    flist.extend_segment(&[h0, h1, h2]);

    flist.sort_segment(0);

    // "a/m.rs" < "b/a.rs" < "b/z.rs"
    let sorted: Vec<(&[u8], &[u8])> = (0..3)
        .map(|i| {
            let e = flist.get(i).unwrap();
            (e.dirname, e.name)
        })
        .collect();
    assert_eq!(
        sorted,
        vec![
            (b"a" as &[u8], b"m.rs" as &[u8]),
            (b"b" as &[u8], b"a.rs" as &[u8]),
            (b"b" as &[u8], b"z.rs" as &[u8]),
        ]
    );
}

#[test]
fn segment_boundary_tracking_accuracy() {
    let mut flist = FlatFileList::new();

    // Build 5 segments with varying sizes.
    let sizes = [3, 1, 4, 0, 2];
    let mut expected_start = 0;

    for (seg_num, &count) in sizes.iter().enumerate() {
        let mut headers = Vec::with_capacity(count);
        for j in 0..count {
            let name = format!("f{j}.txt");
            let dirname = format!("seg{seg_num}");
            headers.push(make_header(&mut flist, &name, &dirname, j as u64));
        }
        let idx = flist.extend_segment(&headers);
        assert_eq!(idx, seg_num, "segment index mismatch");
        expected_start += count;
    }

    assert_eq!(flist.segment_count(), 5);
    assert_eq!(flist.len(), 10); // 3+1+4+0+2

    // Verify each segment's boundaries.
    let mut offset = 0;
    for (i, &count) in sizes.iter().enumerate() {
        let seg = flist.segment(i).unwrap();
        assert_eq!(seg.start_index, offset, "segment {i} start_index wrong");
        assert_eq!(seg.count, count, "segment {i} count wrong");
        assert_eq!(
            flist.segment_range(i),
            Some(offset..offset + count),
            "segment {i} range wrong"
        );
        offset += count;
    }

    // Out-of-bounds access.
    assert!(flist.segment(5).is_none());
    assert!(flist.segment_range(5).is_none());
}

#[test]
fn segment_empty_segment_allowed() {
    let mut flist = FlatFileList::new();

    let h0 = make_header(&mut flist, "before.txt", "", 10);
    flist.extend_segment(&[h0]);

    // Empty segment is valid (a directory with no children).
    let idx = flist.extend_segment(&[]);
    assert_eq!(idx, 1);

    let h1 = make_header(&mut flist, "after.txt", "", 20);
    flist.extend_segment(&[h1]);

    assert_eq!(flist.segment_count(), 3);
    assert_eq!(flist.len(), 2);

    let empty_seg = flist.segment(1).unwrap();
    assert_eq!(empty_seg.start_index, 1);
    assert_eq!(empty_seg.count, 0);
    assert_eq!(flist.segment_range(1), Some(1..1));

    // Entries around the empty segment are unaffected.
    assert_eq!(flist.get(0).unwrap().name, b"before.txt");
    assert_eq!(flist.get(1).unwrap().name, b"after.txt");
}

#[test]
fn segment_no_segments_by_default() {
    let flist = FlatFileList::new();
    assert_eq!(flist.segment_count(), 0);
    assert!(flist.segments().is_empty());
    assert!(flist.segment(0).is_none());
    assert!(flist.segment_range(0).is_none());
}

#[test]
fn sort_range_independent_of_segments() {
    let mut flist = FlatFileList::new();

    // Push entries without using extend_segment.
    push_entry(&mut flist, "cherry", "", 3);
    push_entry(&mut flist, "apple", "", 1);
    push_entry(&mut flist, "banana", "", 2);
    push_entry(&mut flist, "date", "", 4);

    // Sort only the middle two entries.
    flist.sort_range(1..3);

    assert_eq!(flist.get(0).unwrap().name, b"cherry");
    assert_eq!(flist.get(1).unwrap().name, b"apple");
    assert_eq!(flist.get(2).unwrap().name, b"banana");
    assert_eq!(flist.get(3).unwrap().name, b"date");

    // Sort all.
    flist.sort_range(0..4);
    let sorted: Vec<&[u8]> = (0..4).map(|i| flist.get(i).unwrap().name).collect();
    assert_eq!(
        sorted,
        vec![b"apple" as &[u8], b"banana", b"cherry", b"date"]
    );
}

#[test]
fn headers_slice_returns_correct_range() {
    let mut flist = FlatFileList::new();

    let h0 = make_header(&mut flist, "a.txt", "", 10);
    let h1 = make_header(&mut flist, "b.txt", "", 20);
    let h2 = make_header(&mut flist, "c.txt", "", 30);
    flist.extend_segment(&[h0, h1, h2]);

    let h3 = make_header(&mut flist, "d.txt", "", 40);
    flist.extend_segment(&[h3]);

    let slice = flist.headers_slice(0..3);
    assert_eq!(slice.len(), 3);
    assert_eq!(slice[0].size, 10);
    assert_eq!(slice[1].size, 20);
    assert_eq!(slice[2].size, 30);

    let slice2 = flist.headers_slice(3..4);
    assert_eq!(slice2.len(), 1);
    assert_eq!(slice2[0].size, 40);
}

#[test]
fn handle_stability_across_large_segment_growth() {
    let mut flist = FlatFileList::new();

    // Segment 0: a small segment whose handles we will verify later.
    let extras0 = FlatExtras {
        checksum: Some(vec![0xDE; 16]),
        ..FlatExtras::default()
    };
    let h0 = make_header_with_extras(&mut flist, "sentinel.rs", "src/core", 42, &extras0);
    flist.extend_segment(&[h0]);

    let sentinel_name_handle = flist.get(0).unwrap().header.name;
    let sentinel_dirname_handle = flist.get(0).unwrap().header.dirname;
    let sentinel_extras_ref = flist.get(0).unwrap().header.extras;

    // Append many segments to force Vec reallocations on all backing stores.
    for seg in 0..50 {
        let mut headers = Vec::new();
        for j in 0..20 {
            let name = format!("file_{seg}_{j}.rs");
            let dirname = format!("pkg_{seg}");
            let h = make_header(&mut flist, &name, &dirname, (seg * 20 + j) as u64);
            headers.push(h);
        }
        flist.extend_segment(&headers);
    }

    assert_eq!(flist.len(), 1 + 50 * 20); // 1001

    // Verify that segment-0's handles still resolve correctly after all
    // the growth.
    assert_eq!(
        flist.paths().resolve(sentinel_name_handle),
        "sentinel.rs"
    );
    assert_eq!(
        flist.paths().resolve(sentinel_dirname_handle),
        "src/core"
    );
    let decoded = flist.extras().decode(sentinel_extras_ref).unwrap().unwrap();
    assert_eq!(decoded.checksum, Some(vec![0xDE; 16]));

    // Verify via get() too.
    let e0 = flist.get(0).unwrap();
    assert_eq!(e0.name, b"sentinel.rs");
    assert_eq!(e0.dirname, b"src/core");
    assert_eq!(e0.header.size, 42);
}
