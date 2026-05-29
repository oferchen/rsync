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
