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
    assert_eq!(sorted, vec![b"apple", b"banana", b"cherry"]);

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
    assert_eq!(e0.name, e1.name);               // Same resolved bytes
    assert_ne!(e0.header.dirname, e1.header.dirname); // Different dirnames
}

#[test]
fn flat_file_list_default_is_new() {
    let flist = FlatFileList::default();
    assert!(flist.is_empty());
    assert_eq!(flist.len(), 0);
}
