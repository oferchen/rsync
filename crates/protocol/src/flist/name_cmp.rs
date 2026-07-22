//! Foundational byte-wise name comparison for file list entries.
//!
//! Ports the upstream `f_name_cmp()` function from `flist.c` (upstream:
//! flist.c:3252). The comparison is performed in two stages, matching
//! upstream's split between `dirname` and `basename`:
//!
//! 1. Compare the parent directory bytes (`dirname`).
//! 2. If equal, compare the leaf name bytes (`basename`).
//!
//! The comparison is a plain unsigned byte compare via `strcmp` in upstream
//! (`flist.c:3366` - `(int)*c1++ - (int)*c2++`), so bytes with the high bit
//! set sort after ASCII. There is no locale awareness and no case folding.
//!
//! This module intentionally implements the simple, foundational comparator
//! without the protocol-29 `t_PATH`/`t_ITEM` directory-vs-file distinction
//! found in `sort.rs`. It is the sort key the parallel-deterministic delete
//! pipeline uses to merge candidate lists with byte-identical ordering to
//! upstream's pre-sort scan.
//!
//! # Upstream Reference
//!
//! - `flist.c:3232` `enum fnc_state { s_DIR, s_SLASH, s_BASE, s_TRAILING }`
//! - `flist.c:3233` `enum fnc_type { t_PATH, t_ITEM }`
//! - `flist.c:3252-3369` `int f_name_cmp(const struct file_struct *f1,
//!   const struct file_struct *f2)`
//! - `flist.c:3366` `(int)*c1++ - (int)*c2++` - the byte-wise compare
//!   operates on `uchar`, so high bytes sort as unsigned.

use std::cmp::Ordering;

use super::FileEntry;
use super::wire_path::path_bytes_to_wire;

/// Compares two file list entries by `(dirname, basename)` in unsigned
/// byte order.
///
/// This is the foundational sort key for the parallel-deterministic-delete
/// pipeline. It ports upstream rsync's `f_name_cmp()` (upstream:
/// flist.c:3252) in its simplest form: dirname first, then basename, with
/// every byte compared as `unsigned char`. No locale awareness, no case
/// folding, no protocol-29 directory-vs-file disambiguation.
///
/// # Ordering Rules
///
/// 1. Compare the wire-format bytes of `a.dirname()` against `b.dirname()`.
/// 2. If equal, compare the wire-format bytes of the entry basenames.
///
/// # Examples
///
/// ```
/// use std::cmp::Ordering;
/// use std::path::PathBuf;
/// use protocol::flist::{FileEntry, f_name_cmp};
///
/// let a = FileEntry::new_file(PathBuf::from("a/x.txt"), 0, 0o644);
/// let b = FileEntry::new_file(PathBuf::from("a/y.txt"), 0, 0o644);
/// assert_eq!(f_name_cmp(&a, &b), Ordering::Less);
/// ```
#[must_use]
pub fn f_name_cmp(a: &FileEntry, b: &FileEntry) -> Ordering {
    let dir_a = path_bytes_to_wire(a.dirname());
    let dir_b = path_bytes_to_wire(b.dirname());
    match dir_a.cmp(&dir_b) {
        Ordering::Equal => {}
        non_eq => return non_eq,
    }

    let base_a = basename_bytes(a);
    let base_b = basename_bytes(b);
    base_a.cmp(&base_b)
}

/// Returns true if two entries share the same `(dirname, basename)` pair.
///
/// Mirrors the equality side of upstream's `f_name_cmp()` so callers can
/// dedupe by name without re-deriving the comparison rules.
///
/// Upstream rsync also treats a directory entry as equal to itself with or
/// without a trailing `/` (the protocol-29 implicit `/`). This helper folds
/// a single trailing `/` on either basename so callers comparing wire-form
/// directory entries get the same equality.
///
/// # Upstream Reference
///
/// - `flist.c:3241-3246` documents the trailing-slash equivalence for
///   directory entries at protocol 29+.
#[must_use]
pub fn name_cmp_eq(a: &FileEntry, b: &FileEntry) -> bool {
    let dir_a = path_bytes_to_wire(a.dirname());
    let dir_b = path_bytes_to_wire(b.dirname());
    if dir_a != dir_b {
        return false;
    }

    let base_a = basename_bytes(a);
    let base_b = basename_bytes(b);
    strip_trailing_slash(&base_a) == strip_trailing_slash(&base_b)
}

/// Extracts the basename bytes (the path component after the final `/`).
///
/// Mirrors upstream's `f->basename` field, which holds the leaf name only.
/// For root-level entries the basename is the full name. The empty path
/// yields empty bytes.
fn basename_bytes(entry: &FileEntry) -> Vec<u8> {
    let name_cow = path_bytes_to_wire(entry.path().as_path());
    let dir_cow = path_bytes_to_wire(entry.dirname());
    let name: &[u8] = &name_cow;
    let dir: &[u8] = &dir_cow;
    if dir.is_empty() {
        return name.to_vec();
    }
    // If the name starts with `<dirname>/`, strip that prefix.
    if name.len() > dir.len() && name.starts_with(dir) && name[dir.len()] == b'/' {
        return name[dir.len() + 1..].to_vec();
    }
    // Fall back to taking the bytes after the final `/`, if any. This covers
    // edge cases where dirname was set independently of the name (e.g.
    // interned to an unrelated `Arc<Path>` in tests).
    match memchr::memrchr(b'/', name) {
        Some(pos) => name[pos + 1..].to_vec(),
        None => name.to_vec(),
    }
}

fn strip_trailing_slash(bytes: &[u8]) -> &[u8] {
    match bytes.split_last() {
        Some((&b'/', rest)) => rest,
        _ => bytes,
    }
}

/// Conservative direct comparator that operates on raw `dirname` and
/// `basename` byte slices. Useful for tests, fuzzers, and callers that
/// already have the components in hand.
#[must_use]
pub fn f_name_cmp_components(dir_a: &[u8], base_a: &[u8], dir_b: &[u8], base_b: &[u8]) -> Ordering {
    match dir_a.cmp(dir_b) {
        Ordering::Equal => base_a.cmp(base_b),
        non_eq => non_eq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flist::FileEntry;
    use proptest::prelude::*;
    use std::path::PathBuf;

    fn file(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 0, 0o644)
    }

    fn dir(name: &str) -> FileEntry {
        FileEntry::new_directory(PathBuf::from(name), 0o755)
    }

    #[test]
    fn plain_ascii_lt() {
        assert_eq!(f_name_cmp(&file("a"), &file("b")), Ordering::Less);
        assert_eq!(f_name_cmp(&file("abc"), &file("abd")), Ordering::Less);
        assert_eq!(f_name_cmp(&file("b"), &file("a")), Ordering::Greater);
        assert_eq!(f_name_cmp(&file("a"), &file("a")), Ordering::Equal);
    }

    #[test]
    fn same_basename_different_dirname() {
        // dirname "a" vs dirname "b", same basename "leaf"
        let a = file("a/leaf");
        let b = file("b/leaf");
        assert_eq!(f_name_cmp(&a, &b), Ordering::Less);
        assert_eq!(f_name_cmp(&b, &a), Ordering::Greater);
    }

    #[test]
    fn same_dirname_different_basename() {
        let a = file("x/a");
        let b = file("x/b");
        assert_eq!(f_name_cmp(&a, &b), Ordering::Less);
    }

    #[cfg(unix)]
    #[test]
    fn high_bytes_unsigned_order() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        // "a<0xC3><0xA9>b" vs "a<0xC2>b" - 0xC3 > 0xC2 as unsigned, so the
        // first sorts after the second. This is upstream's behaviour because
        // it casts to uchar before subtracting (upstream: flist.c:3366).
        let a = FileEntry::new_file(PathBuf::from(OsStr::from_bytes(b"a\xC3\xA9b")), 0, 0o644);
        let b = FileEntry::new_file(PathBuf::from(OsStr::from_bytes(b"a\xC2b")), 0, 0o644);
        assert_eq!(f_name_cmp(&a, &b), Ordering::Greater);
        assert_eq!(f_name_cmp(&b, &a), Ordering::Less);
    }

    #[test]
    fn high_bytes_unsigned_order_components() {
        // Components-level check, portable: 0xFF must sort after 0x7F.
        assert_eq!(
            f_name_cmp_components(b"", b"\xFF", b"", b"\x7F"),
            Ordering::Greater,
        );
    }

    #[test]
    fn dot_and_double_dot() {
        // "." vs ".." - byte-wise, "." (0x2E) is a prefix of ".." so "." < "..".
        let dot = dir(".");
        let dotdot = dir("..");
        assert_eq!(f_name_cmp(&dot, &dotdot), Ordering::Less);
    }

    #[test]
    fn empty_dirname_root_files() {
        // Root-level entries have an empty dirname; basenames sort directly.
        let a = file("readme");
        let b = file("zebra");
        assert_eq!(f_name_cmp(&a, &b), Ordering::Less);
    }

    #[test]
    fn hidden_files_start_with_dot() {
        // ".hidden" (0x2E) sorts before "visible" (0x76) because 0x2E < 0x76.
        let hidden = file(".hidden");
        let visible = file("visible");
        assert_eq!(f_name_cmp(&hidden, &visible), Ordering::Less);
    }

    #[test]
    fn name_cmp_eq_trailing_slash_tolerated_on_basename() {
        // The components helper makes the trailing-slash equivalence
        // explicit: a basename "subdir" and "subdir/" compare equal under
        // name_cmp_eq. This mirrors upstream's protocol-29 implicit
        // trailing-`/` rule for directory entries (upstream:
        // flist.c:3241-3246).
        assert_eq!(strip_trailing_slash(b"subdir/"), b"subdir");
        assert_eq!(strip_trailing_slash(b"subdir"), b"subdir");
        assert_eq!(strip_trailing_slash(b""), b"");
    }

    #[test]
    fn name_cmp_eq_distinct_names_not_equal() {
        assert!(!name_cmp_eq(&file("a"), &file("b")));
        assert!(!name_cmp_eq(&file("x/a"), &file("y/a")));
        assert!(!name_cmp_eq(&file("x/a"), &file("x/b")));
    }

    #[test]
    fn name_cmp_eq_same_full_path() {
        assert!(name_cmp_eq(&file("a/b/c"), &file("a/b/c")));
    }

    #[test]
    fn embedded_slashes_treated_as_byte_slice() {
        // Two entries with the same dirname but basenames whose ordering
        // differs by an embedded byte. The comparator never re-splits the
        // basename - it is treated as an opaque byte run.
        let a = file("dir/a-b");
        let b = file("dir/a.b");
        // '-' (0x2D) < '.' (0x2E)
        assert_eq!(f_name_cmp(&a, &b), Ordering::Less);
    }

    #[test]
    fn dirname_ordering_dominates_basename() {
        // Different dirname, basename "a" vs "z": dirname decides.
        let a = file("aaa/z");
        let b = file("bbb/a");
        assert_eq!(f_name_cmp(&a, &b), Ordering::Less);
    }

    #[test]
    fn deeply_nested_paths_compare_dirname_first() {
        let a = file("a/b/c/leaf");
        let b = file("a/b/d/leaf");
        assert_eq!(f_name_cmp(&a, &b), Ordering::Less);
    }

    #[test]
    fn directories_and_files_compare_by_bytes_only() {
        // This comparator deliberately does NOT implement t_PATH vs t_ITEM
        // (that lives in sort.rs). With identical names, a file and a dir
        // compare equal here.
        assert_eq!(f_name_cmp(&file("same"), &dir("same")), Ordering::Equal);
    }

    // ----- proptest: total-order properties -----

    fn arb_name() -> impl Strategy<Value = String> {
        // ASCII printable plus '/' separator, length 0..16.
        prop::collection::vec(any::<u8>().prop_map(|b| ((b % 95) + 32) as char), 0..16)
            .prop_map(|chars| chars.into_iter().collect::<String>())
            .prop_filter("non-empty basename required", |s| {
                // Avoid trailing slash producing an empty basename for FileEntry.
                !s.is_empty() && !s.ends_with('/') && s != "."
            })
    }

    fn arb_entry() -> impl Strategy<Value = FileEntry> {
        arb_name().prop_map(|n| FileEntry::new_file(PathBuf::from(n), 0, 0o644))
    }

    proptest! {
        #[test]
        fn antisymmetry(a in arb_entry(), b in arb_entry()) {
            let ab = f_name_cmp(&a, &b);
            let ba = f_name_cmp(&b, &a);
            prop_assert_eq!(ab, ba.reverse());
        }

        #[test]
        fn reflexivity(a in arb_entry()) {
            prop_assert_eq!(f_name_cmp(&a, &a), Ordering::Equal);
        }

        #[test]
        fn transitivity(a in arb_entry(), b in arb_entry(), c in arb_entry()) {
            let ab = f_name_cmp(&a, &b);
            let bc = f_name_cmp(&b, &c);
            if ab != Ordering::Greater && bc != Ordering::Greater {
                prop_assert_ne!(f_name_cmp(&a, &c), Ordering::Greater);
            }
            if ab != Ordering::Less && bc != Ordering::Less {
                prop_assert_ne!(f_name_cmp(&a, &c), Ordering::Less);
            }
        }

        #[test]
        fn agrees_with_components_helper(a in arb_entry(), b in arb_entry()) {
            let dir_a = path_bytes_to_wire(a.dirname());
            let dir_b = path_bytes_to_wire(b.dirname());
            let base_a = basename_bytes(&a);
            let base_b = basename_bytes(&b);
            let direct = f_name_cmp_components(&dir_a, &base_a, &dir_b, &base_b);
            prop_assert_eq!(direct, f_name_cmp(&a, &b));
        }
    }
}
