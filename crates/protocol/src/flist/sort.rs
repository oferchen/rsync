//! File list sorting for rsync protocol.
//!
//! Both sender and receiver must sort their file lists identically after
//! building/receiving them. This ensures NDX (file index) values match
//! on both sides.
//!
//! # Upstream Reference
//!
//! - `flist.c:flist_sort_and_clean()` - Sorts file list after build/receive
//! - `flist.c:f_name_cmp()` - File entry comparison function
//!
//! The sorting algorithm follows these rules (protocol 29+):
//! 1. "." (root directory marker) always comes first
//! 2. Files sort before directories at the same level
//! 3. Within each category (files or directories), sort alphabetically
//! 4. Directory contents immediately follow the directory entry
//!
//! At protocol < 29, upstream uses plain lexicographic byte comparison
//! without file-before-directory distinction or implicit trailing '/'.
//! upstream: flist.c:3223 - `protocol_version >= 29 ? t_PATH : t_ITEM`

use std::cmp::Ordering;

use logging::debug_log;
use memchr::memrchr;

use super::FileEntry;

/// Cached sort metadata for a file entry, precomputed once before sorting.
///
/// Avoids per-comparison `memrchr` and `is_dir()` calls during the O(n log n)
/// sort phase. For 100K entries (~1.7M comparisons), this eliminates ~3.4M
/// `memrchr` calls and ~3.4M mode bitmask checks.
#[derive(Clone, Copy)]
struct SortKey {
    index: u32,
    is_dir: bool,
    /// Precomputed position of last '/' in name bytes, or `u32::MAX` if none.
    last_slash: u32,
}

impl SortKey {
    fn new(index: usize, entry: &FileEntry) -> Self {
        let bytes = entry.name_bytes();
        let last_slash = memrchr(b'/', &bytes).map_or(u32::MAX, |p| p as u32);
        Self {
            index: index as u32,
            is_dir: entry.is_dir(),
            last_slash,
        }
    }

    fn has_slash_at_or_after(&self, pos: usize) -> bool {
        self.last_slash != u32::MAX && self.last_slash as usize >= pos
    }
}

/// Compares two file entries according to rsync's sorting rules.
///
/// This mirrors upstream's `f_name_cmp()` from `flist.c`.
///
/// # Sorting Rules (Protocol 29+)
///
/// 1. "." always sorts first (root directory marker)
/// 2. At each directory depth, non-directories (files, symlinks, etc.) sort BEFORE directories
/// 3. Directories are compared as if they have a trailing '/'
/// 4. Within the same type, sort by byte comparison
/// 5. Directory contents follow the directory entry
///
/// # Total Order Guarantee
///
/// This function implements a total order by using a canonical comparison
/// that is transitive: if a < b and b < c, then a < c.
///
/// # Performance
///
/// At the divergence point, the comparator must determine whether each entry
/// still has deeper path components (i.e., a '/' exists at or after position `i`).
/// Instead of scanning forward from `i` on every divergence (O(remaining_length)),
/// we precompute the last '/' position via `memrchr` once per call and answer
/// the query in O(1): `last_slash >= i` means a separator remains.
/// This matches upstream's approach of avoiding forward scans in `f_name_cmp()`
/// (upstream: flist.c:3217).
#[must_use]
pub fn compare_file_entries(a: &FileEntry, b: &FileEntry) -> Ordering {
    let key_a = SortKey::new(0, a);
    let key_b = SortKey::new(0, b);
    let bytes_a = a.name_bytes();
    let bytes_b = b.name_bytes();
    compare_with_keys(&bytes_a, &key_a, &bytes_b, &key_b)
}

/// Protocol < 29 comparison: plain byte-for-byte comparison without
/// file-before-directory distinction or implicit trailing '/'.
///
/// At protocol < 29, upstream `f_name_cmp()` uses `t_path = t_ITEM`,
/// meaning directories are NOT treated specially - no implicit trailing
/// slash, no files-before-dirs. This is a simple lexicographic sort.
/// upstream: flist.c:3223 - `protocol_version >= 29 ? t_PATH : t_ITEM`
fn compare_with_keys_pre29(bytes_a: &[u8], bytes_b: &[u8]) -> Ordering {
    // "." always comes first (even at protocol < 29)
    match (bytes_a == b".", bytes_b == b".") {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        (false, false) => {}
    }

    // Plain byte comparison - no file-before-dir, no implicit trailing '/'.
    // upstream: f_name_cmp() with t_path = t_ITEM treats all entries identically.
    bytes_a.cmp(bytes_b)
}

/// Inner comparison using precomputed sort keys.
///
/// Separated from `compare_file_entries` so the sort loop can pass cached
/// `SortKey` values without recomputing `memrchr` and `is_dir` per call.
fn compare_with_keys(bytes_a: &[u8], key_a: &SortKey, bytes_b: &[u8], key_b: &SortKey) -> Ordering {
    // "." always comes first
    match (bytes_a == b".", bytes_b == b".") {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        (false, false) => {}
    }

    let a_is_dir = key_a.is_dir;
    let b_is_dir = key_b.is_dir;

    // Compare byte by byte, treating directory names as having an implicit
    // trailing '/' (so `dir` compares as `dir/`).
    let mut i = 0;
    loop {
        let ch_a = if i < bytes_a.len() {
            bytes_a[i]
        } else if i == bytes_a.len() && a_is_dir {
            b'/'
        } else {
            0
        };

        let ch_b = if i < bytes_b.len() {
            bytes_b[i]
        } else if i == bytes_b.len() && b_is_dir {
            b'/'
        } else {
            0
        };

        let a_done = i > bytes_a.len() || (i == bytes_a.len() && !a_is_dir);
        let b_done = i > bytes_b.len() || (i == bytes_b.len() && !b_is_dir);

        if a_done && b_done {
            return Ordering::Equal;
        }
        if a_done {
            return Ordering::Less;
        }
        if b_done {
            return Ordering::Greater;
        }

        if ch_a != ch_b {
            // At the divergence point, determine if each entry has deeper path
            // components remaining. Uses precomputed last_slash for O(1) lookup.
            let a_has_sep = key_a.has_slash_at_or_after(i);
            let b_has_sep = key_b.has_slash_at_or_after(i);

            // Entry is "directory-like at this level" if it has more path components
            // (separator found ahead) or if it's a directory entry in its final component
            let a_is_dir_here = a_has_sep || a_is_dir;
            let b_is_dir_here = b_has_sep || b_is_dir;

            // At each level, files sort before directories
            match (a_is_dir_here, b_is_dir_here) {
                (true, false) => return Ordering::Greater, // a is dir, b is file -> b first
                (false, true) => return Ordering::Less,    // a is file, b is dir -> a first
                _ => {}                                    // Same type, compare bytes
            }

            // Same type at this level - compare the effective bytes
            return ch_a.cmp(&ch_b);
        }

        i += 1;
    }
}

/// Sorts a file list in-place according to rsync's sorting rules.
///
/// Uses index-based sorting to minimize memory traffic: only 8-byte
/// indices are shuffled during comparisons, then a single permutation
/// pass moves the ~160-byte `FileEntry` values into their final positions.
/// This mirrors upstream rsync's approach of sorting a pointer array
/// (`sorted[]`) rather than moving `file_struct` data.
///
/// When `use_qsort` is true, uses an unstable sort (matching upstream's
/// `--qsort` which uses the C library `qsort()`). When false, uses a
/// stable merge sort (upstream's default).
///
/// # Upstream Reference
///
/// - `flist.c:flist_sort_and_clean()` - Called after `send_file_list()`
///   and `recv_file_list()` to sort entries.
/// - `flist.c:2991` - `if (use_qsort) qsort(...); else merge_sort(...);`
pub fn sort_file_list(file_list: &mut [FileEntry], use_qsort: bool, protocol_pre29: bool) {
    debug_log!(
        Flist,
        2,
        "sorting {} entries (pre29={})",
        file_list.len(),
        protocol_pre29
    );
    let n = file_list.len();
    if n <= 1 {
        return;
    }

    // Precompute sort keys (is_dir + last_slash) once per entry.
    // Eliminates ~3.4M memrchr calls for 100K entries (~1.7M comparisons * 2).
    let mut keys: Vec<SortKey> = file_list
        .iter()
        .enumerate()
        .map(|(i, e)| SortKey::new(i, e))
        .collect();

    if protocol_pre29 {
        // Protocol < 29: plain lexicographic sort, no file-before-dir.
        // upstream: flist.c:3223 - t_path = t_ITEM at protocol < 29.
        let cmp = |a: &SortKey, b: &SortKey| {
            let bytes_a = file_list[a.index as usize].name_bytes();
            let bytes_b = file_list[b.index as usize].name_bytes();
            compare_with_keys_pre29(&bytes_a, &bytes_b)
        };
        if use_qsort {
            keys.sort_unstable_by(cmp);
        } else {
            keys.sort_by(cmp);
        }
    } else {
        let cmp = |a: &SortKey, b: &SortKey| {
            let bytes_a = file_list[a.index as usize].name_bytes();
            let bytes_b = file_list[b.index as usize].name_bytes();
            compare_with_keys(&bytes_a, a, &bytes_b, b)
        };
        if use_qsort {
            keys.sort_unstable_by(cmp);
        } else {
            keys.sort_by(cmp);
        }
    }

    // Apply the permutation in-place using cycle chasing.
    // Each element is moved exactly once. Uses a bitset (n/64 u64s)
    // instead of Vec<bool> (n bytes) to reduce memory and improve
    // cache behavior for the placed-tracking.
    let mut placed = vec![0u64; n.div_ceil(64)];
    for i in 0..n {
        let word = i / 64;
        let bit = 1u64 << (i % 64);
        let idx = keys[i].index as usize;
        if placed[word] & bit != 0 || idx == i {
            placed[word] |= bit;
            continue;
        }
        let mut j = i;
        loop {
            let target = keys[j].index as usize;
            let jw = j / 64;
            let jb = 1u64 << (j % 64);
            placed[jw] |= jb;
            if target == i {
                break;
            }
            file_list.swap(j, target);
            j = target;
        }
    }
}

/// Result of cleaning a file list.
#[derive(Debug, Clone, Default)]
pub struct CleanResult {
    /// Number of duplicate entries removed.
    pub duplicates_removed: usize,
    /// Number of directory flags merged.
    pub flags_merged: usize,
}

/// Resolves a duplicate-name pair during a file-list clean, single-sourcing the
/// upstream tie-break for both the receiver's [`flist_clean`] and the sender's
/// [`super::dual::DualFileList::dedup_with_parallel`].
///
/// `write` is the currently-kept entry, `read` the later duplicate. Returns
/// `true` when the caller must replace `write` with `read` (take the read
/// entry), `false` to keep `write`. Updates `stats` for the removed duplicate
/// and any merged directory flag.
///
/// # Upstream Reference
///
/// - `flist.c:3050-3082` - "If one is a dir and the other is not, we want to
///   keep the dir because it might have contents in the list. Otherwise keep
///   the first one." When both are dirs, upstream merges the vital flags into
///   the survivor (`fp->flags |= file->flags & (FLAG_TOP_DIR|FLAG_CONTENT_DIR)`).
pub(super) fn resolve_duplicate(
    write: &mut FileEntry,
    read: &FileEntry,
    stats: &mut CleanResult,
) -> bool {
    stats.duplicates_removed += 1;
    match (write.is_dir(), read.is_dir()) {
        // Keep the directory (read) over the plain file (write).
        (false, true) => true,
        // Keep the directory (write) over the plain file (read).
        (true, false) => false,
        // Both directories - keep the first, merge the survivor's vital dir
        // flags from the dropped duplicate. upstream: flist.c:3073-3076
        // (!am_sender) `fp->flags |= file->flags & (FLAG_TOP_DIR|FLAG_CONTENT_DIR)`.
        // TOP_DIR scopes --delete, so a surviving duplicate that lost it could
        // wrongly become delete-eligible. (oc collapses upstream's separate
        // FLAG_IMPLIED_DIR into the top_dir/content_dir state at read time, so
        // only these two flags are represented here.)
        (true, true) => {
            if read.top_dir() {
                write.set_top_dir(true);
            }
            if read.content_dir() {
                write.set_content_dir(true);
            }
            stats.flags_merged += 1;
            false
        }
        // Both plain entries - keep the first.
        (false, false) => false,
    }
}

/// Scans backward from a directory entry for an active, same-named non-dir
/// earlier in the sorted list.
///
/// The primary adjacent-name check catches a directory whose same-named non-dir
/// twin sorts immediately before it. This handles the non-adjacent case: files
/// sort before directories, but an entry such as `item!` (a byte that sorts
/// before `/`) can sit between the exact same-named non-dir `item` and the
/// directory `item` (which sorts as `item/`). Only names equal to this dir's
/// name, or extending it with a byte that sorts before `/`, can lie in that
/// span, so the scan stops once it leaves the zone. Tombstoned slots keep their
/// position and are skipped without ending the scan.
///
/// # Upstream Reference
///
/// - `flist.c:3052-3059` - "Make sure that this directory doesn't duplicate a
///   non-directory earlier in the list." Upstream temporarily sets
///   `file->mode = S_IFREG` and calls `flist_find()` (a binary search over the
///   sorted array) to locate the twin.
fn find_regfile_dup(file_list: &[FileEntry], dir_idx: usize) -> Option<usize> {
    let dir_name = file_list[dir_idx].name_bytes();
    let dir_name = dir_name.as_ref();
    let mut k = dir_idx;
    while k > 0 {
        k -= 1;
        // Tombstones keep their slot; skip them and keep scanning.
        if !file_list[k].is_active() {
            continue;
        }
        let name = file_list[k].name_bytes();
        let name = name.as_ref();
        if name == dir_name {
            if !file_list[k].is_dir() {
                return Some(k);
            }
            // A same-named dir is handled via the adjacent-name path; keep
            // scanning past it for an earlier non-dir twin.
            continue;
        }
        // Leave the zone once the name is neither the dir's name nor an
        // extension of it by a byte that sorts before '/'.
        let extends_before_slash = name.len() > dir_name.len()
            && name.starts_with(dir_name)
            && name[dir_name.len()] < b'/';
        if !extends_before_slash {
            break;
        }
    }
    None
}

/// Cleans a sorted file list in-place by tombstoning duplicate names.
///
/// Mirrors upstream's duplicate-clean pass inside `flist_sort_and_clean()`.
/// The dropped duplicate is cleared in place (see [`FileEntry::tombstone`]) so
/// the array length and every NDX (file index) slot are preserved: the
/// receiver's numbering stays aligned with the sender's full un-deduped array.
/// The list is NOT compacted, truncated, or renumbered. Consumers iterate the
/// list and skip inactive slots.
///
/// # Sender vs receiver
///
/// A non-incremental sender (`am_sender && !inc_recurse`) skips the pass
/// entirely and transmits every entry as-is; the receiver's tombstones then
/// keep both sides aligned. The receiver (`am_sender == false`) always runs
/// the pass.
///
/// # Duplicate Handling Rules
///
/// When duplicate names are found:
/// 1. If one is a directory and the other isn't, keep the directory
///    (it may have contents in the list).
/// 2. If both are directories, keep the first and merge flags.
/// 3. Otherwise, keep the first entry.
///
/// # Arguments
///
/// * `file_list` - A sorted file list (call `sort_file_list` first).
/// * `am_sender` - `true` on the sending side.
/// * `inc_recurse` - `true` when INC_RECURSE is negotiated.
///
/// # Returns
///
/// A tuple of `(cleaned_list, CleanResult)` where `cleaned_list` has the same
/// length as the input, dropped duplicates tombstoned, and `CleanResult`
/// carries statistics.
///
/// # Upstream Reference
///
/// - `flist.c:3016-3104 flist_sort_and_clean()` - the sort + duplicate-clean.
/// - `flist.c:3031-3042` - `am_sender && !inc_recurse` skips the clean loop.
/// - `flist.c:3089 clear_file()` - the receiver tombstones the dropped slot.
#[must_use]
pub fn flist_clean(
    mut file_list: Vec<FileEntry>,
    am_sender: bool,
    inc_recurse: bool,
) -> (Vec<FileEntry>, CleanResult) {
    let len = file_list.len();
    let mut stats = CleanResult::default();
    if len == 0 {
        return (file_list, stats);
    }

    // upstream: flist.c:3039-3042 - a non-incremental sender sets `i = used - 1`
    // so the clean loop never runs; it transmits duplicates as-is so the
    // receiver's tombstones keep NDX aligned with this full array.
    if am_sender && !inc_recurse {
        return (file_list, stats);
    }

    // upstream: flist.c:3032-3038 - anchor `prev` on the first active entry.
    // A freshly sorted list has no tombstones, but scan defensively.
    let mut prev = 0usize;
    while prev < len && !file_list[prev].is_active() {
        prev += 1;
    }
    if prev >= len {
        return (file_list, stats);
    }

    let mut i = prev + 1;
    while i < len {
        if !file_list[i].is_active() {
            i += 1;
            continue;
        }

        // upstream: flist.c:3050-3061 - a duplicate is either the same name as
        // the previous kept entry, or (for a directory) an earlier same-named
        // non-dir found via flist_find().
        let dup = if file_list[i].name() == file_list[prev].name() {
            Some(prev)
        } else if file_list[i].is_dir() {
            find_regfile_dup(&file_list, i)
        } else {
            None
        };

        let Some(j) = dup else {
            prev = i;
            i += 1;
            continue;
        };

        // upstream: flist.c:3062-3090 - keep the directory over a plain file
        // (it may have contents in the list), else keep the first; the receiver
        // tombstones the dropped slot in place. `resolve_duplicate` returns
        // `true` when the later entry `i` wins.
        let (left, right) = file_list.split_at_mut(i);
        let take_read = resolve_duplicate(&mut left[j], &right[0], &mut stats);
        if take_read {
            file_list[j].tombstone();
            prev = i;
        } else {
            file_list[i].tombstone();
        }
        i += 1;
    }

    debug_log!(
        Flist,
        2,
        "cleaned file list: {} slots, {} duplicates tombstoned, {} flags merged",
        len,
        stats.duplicates_removed,
        stats.flags_merged
    );

    (file_list, stats)
}

/// Sorts and cleans a file list in one operation.
///
/// Combines `sort_file_list` and `flist_clean` for convenience.
/// When `use_qsort` is true, uses unstable sort matching upstream `--qsort`.
///
/// # Upstream Reference
///
/// - `flist.c:flist_sort_and_clean()` - The combined operation
#[must_use]
pub fn sort_and_clean_file_list(
    mut file_list: Vec<FileEntry>,
    use_qsort: bool,
    protocol_pre29: bool,
    am_sender: bool,
    inc_recurse: bool,
) -> (Vec<FileEntry>, CleanResult) {
    sort_file_list(&mut file_list, use_qsort, protocol_pre29);
    flist_clean(file_list, am_sender, inc_recurse)
}

/// Apply a precomputed sort permutation to two parallel slices in lockstep.
///
/// `source_indices[i] = j` encodes "the entry at position `j` in the source
/// becomes the entry at position `i` after sorting." Both slices must have
/// equal length matching `source_indices.len()`. Used by the generator's
/// file-list sort to reorder a [`FileEntry`] (or arena header) slice alongside
/// a parallel `Vec<PathBuf>` so both stay aligned after an indirect sort.
///
/// Cycle-following algorithm performs `O(n)` swaps and allocates a single
/// destination-permutation vector of length `n`; no per-element clones.
///
/// # Upstream Reference
///
/// - `flist.c:f_name_cmp()` - upstream sorts the file list in-place;
///   we sort via indirect permutation to avoid `O(n)` clones of `FileEntry`.
pub fn apply_permutation_in_place<A, B>(
    slice_a: &mut [A],
    slice_b: &mut [B],
    source_indices: Vec<usize>,
) {
    let n = slice_a.len();
    debug_assert_eq!(slice_b.len(), n);
    debug_assert_eq!(source_indices.len(), n);

    if n == 0 {
        return;
    }

    let mut dest_perm = vec![0; n];
    for (new_pos, &old_pos) in source_indices.iter().enumerate() {
        dest_perm[old_pos] = new_pos;
    }

    for i in 0..n {
        while dest_perm[i] != i {
            let j = dest_perm[i];
            slice_a.swap(i, j);
            slice_b.swap(i, j);
            dest_perm.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(name: &str) -> FileEntry {
        FileEntry::new_file(name.into(), 0, 0o644)
    }

    fn make_dir(name: &str) -> FileEntry {
        FileEntry::new_directory(name.into(), 0o755)
    }

    #[test]
    fn dot_always_first() {
        let dot = make_dir(".");
        let file = make_file("abc.txt");
        assert_eq!(compare_file_entries(&dot, &file), Ordering::Less);
        assert_eq!(compare_file_entries(&file, &dot), Ordering::Greater);
    }

    #[test]
    fn files_before_dirs_at_same_level() {
        let file = make_file("zebra.txt");
        let dir = make_dir("aardvark");
        // Even though 'a' < 'z' alphabetically, files come before dirs
        assert_eq!(compare_file_entries(&file, &dir), Ordering::Less);
    }

    #[test]
    fn alphabetical_within_files() {
        let a = make_file("a.txt");
        let b = make_file("b.txt");
        assert_eq!(compare_file_entries(&a, &b), Ordering::Less);
    }

    #[test]
    fn alphabetical_within_dirs() {
        let a = make_dir("adir");
        let b = make_dir("bdir");
        assert_eq!(compare_file_entries(&a, &b), Ordering::Less);
    }

    #[test]
    fn dir_contents_follow_dir() {
        let dir = make_dir("subdir");
        let file_in_dir = make_file("subdir/file.txt");
        assert_eq!(compare_file_entries(&dir, &file_in_dir), Ordering::Less);
    }

    #[test]
    fn nested_dir_contents() {
        let parent = make_dir("a");
        let child = make_dir("a/b");
        let grandchild = make_file("a/b/c.txt");
        assert_eq!(compare_file_entries(&parent, &child), Ordering::Less);
        assert_eq!(compare_file_entries(&child, &grandchild), Ordering::Less);
    }

    #[test]
    fn sort_mixed_entries() {
        let mut entries = vec![
            make_file("test.txt"),
            make_dir("subdir"),
            make_file("subdir/file.txt"),
            make_file("another.txt"),
            make_dir("."),
        ];

        sort_file_list(&mut entries, false, false);

        let mut names = Vec::with_capacity(entries.len());
        names.extend(entries.iter().map(|e| e.name()));
        assert_eq!(
            names,
            vec![".", "another.txt", "test.txt", "subdir", "subdir/file.txt"]
        );
    }

    #[test]
    fn sort_files_at_root_before_nested() {
        let mut entries = vec![make_file("z.txt"), make_file("a/nested.txt"), make_dir("a")];

        sort_file_list(&mut entries, false, false);

        let mut names = Vec::with_capacity(entries.len());
        names.extend(entries.iter().map(|e| e.name()));
        // z.txt is a file at root, so it comes before the 'a' directory
        assert_eq!(names, vec!["z.txt", "a", "a/nested.txt"]);
    }

    // flist_clean tests

    /// Names of the active (non-tombstone) entries, in array order.
    fn active_names(list: &[FileEntry]) -> Vec<&str> {
        list.iter()
            .filter(|e| e.is_active())
            .map(FileEntry::name)
            .collect()
    }

    #[test]
    fn flist_clean_empty_list() {
        let entries: Vec<FileEntry> = vec![];
        let (cleaned, stats) = flist_clean(entries, false, false);
        assert!(cleaned.is_empty());
        assert_eq!(stats.duplicates_removed, 0);
        assert_eq!(stats.flags_merged, 0);
    }

    #[test]
    fn flist_clean_no_duplicates() {
        let entries = vec![make_file("a.txt"), make_file("b.txt"), make_dir("c")];
        let (cleaned, stats) = flist_clean(entries, false, false);
        assert_eq!(cleaned.len(), 3);
        assert!(cleaned.iter().all(FileEntry::is_active));
        assert_eq!(stats.duplicates_removed, 0);
    }

    /// The receiver must TOMBSTONE dropped duplicates in place, preserving the
    /// array length and every NDX slot, rather than compacting and renumbering.
    /// A shorter list would desync the receiver's NDX from an upstream sender's
    /// full un-deduped array (received "non-regular file" / silent corruption).
    /// upstream: flist.c:3089 clear_file() drops the slot without moving others.
    #[test]
    fn flist_clean_tombstones_file_duplicates_in_place() {
        // Two files with same name - keep first, tombstone the second slot.
        let entries = vec![make_file("a.txt"), make_file("a.txt"), make_file("b.txt")];
        let (cleaned, stats) = flist_clean(entries, false, false);
        // Length and NDX slots preserved.
        assert_eq!(cleaned.len(), 3);
        assert_eq!(stats.duplicates_removed, 1);
        // Slot 0 kept, slot 1 tombstoned, slot 2 kept.
        assert!(cleaned[0].is_active());
        assert_eq!(cleaned[0].name(), "a.txt");
        assert!(!cleaned[1].is_active());
        assert!(cleaned[2].is_active());
        assert_eq!(cleaned[2].name(), "b.txt");
        assert_eq!(active_names(&cleaned), vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn flist_clean_keeps_dir_over_file() {
        // Directory vs file with same name - keep directory, tombstone the file.
        let entries = vec![make_file("item"), make_dir("item")];
        let (cleaned, stats) = flist_clean(entries, false, false);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(stats.duplicates_removed, 1);
        // The file slot (0) is tombstoned; the dir survives at slot 1 so its
        // NDX still matches the sender's directory entry.
        assert!(!cleaned[0].is_active());
        assert!(cleaned[1].is_active());
        assert!(cleaned[1].is_dir());
    }

    #[test]
    fn flist_clean_keeps_dir_over_file_reverse_order() {
        // Directory first, then file with same name - still keep the directory.
        let entries = vec![make_dir("item"), make_file("item")];
        let (cleaned, stats) = flist_clean(entries, false, false);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(stats.duplicates_removed, 1);
        assert!(cleaned[0].is_active());
        assert!(cleaned[0].is_dir());
        assert!(!cleaned[1].is_active());
    }

    #[test]
    fn flist_clean_merges_directory_flags() {
        // Two directories with same name - merge flags, keep first.
        let mut dir1 = make_dir("subdir");
        dir1.set_content_dir(false);
        let dir2 = make_dir("subdir"); // content_dir is true by default
        let entries = vec![dir1, dir2];
        let (cleaned, stats) = flist_clean(entries, false, false);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(stats.duplicates_removed, 1);
        assert_eq!(stats.flags_merged, 1);
        // Survivor at slot 0 with the merged content-dir flag; slot 1 tombstoned.
        assert!(cleaned[0].is_active());
        assert!(cleaned[0].content_dir());
        assert!(!cleaned[1].is_active());
    }

    #[test]
    fn flist_clean_merges_top_dir_flag() {
        // A duplicate directory carrying TOP_DIR must pass that flag to the
        // survivor: TOP_DIR scopes --delete, so dropping it on merge would
        // wrongly make the surviving directory eligible for deletion.
        // upstream: flist.c:3073 `fp->flags |= file->flags & FLAG_TOP_DIR`.
        let mut dir1 = make_dir("subdir");
        dir1.set_top_dir(false);
        let mut dir2 = make_dir("subdir");
        dir2.set_top_dir(true);
        let entries = vec![dir1, dir2];
        let (cleaned, stats) = flist_clean(entries, false, false);
        assert_eq!(stats.flags_merged, 1);
        assert!(cleaned[0].is_active());
        assert!(cleaned[0].top_dir(), "survivor must inherit TOP_DIR");
        assert!(!cleaned[1].is_active());
    }

    #[test]
    fn flist_clean_multiple_duplicates() {
        let entries = vec![
            make_file("a.txt"),
            make_file("a.txt"),
            make_file("a.txt"),
            make_file("b.txt"),
        ];
        let (cleaned, stats) = flist_clean(entries, false, false);
        // Length preserved; two slots tombstoned.
        assert_eq!(cleaned.len(), 4);
        assert_eq!(stats.duplicates_removed, 2);
        assert!(cleaned[0].is_active());
        assert!(!cleaned[1].is_active());
        assert!(!cleaned[2].is_active());
        assert!(cleaned[3].is_active());
        assert_eq!(active_names(&cleaned), vec!["a.txt", "b.txt"]);
    }

    /// A non-incremental SENDER must skip the clean pass entirely and transmit
    /// duplicates as-is; otherwise it would ship fewer entries than the receiver
    /// tombstones, desyncing the wire NDX. upstream: flist.c:3039-3042.
    #[test]
    fn flist_clean_sender_noninc_skips_dedup() {
        let entries = vec![make_file("dup"), make_file("dup"), make_file("z")];
        let (kept, stats) = flist_clean(entries, true, false);
        assert_eq!(stats.duplicates_removed, 0);
        // Nothing tombstoned: all three transmitted as-is.
        assert_eq!(kept.len(), 3);
        assert!(kept.iter().all(FileEntry::is_active));
        assert_eq!(active_names(&kept), vec!["dup", "dup", "z"]);
    }

    /// An incremental sender still cleans (upstream `!am_sender || inc_recurse`).
    #[test]
    fn flist_clean_sender_inc_recurse_still_cleans() {
        let entries = vec![make_file("dup"), make_file("dup"), make_file("z")];
        let (cleaned, stats) = flist_clean(entries, true, true);
        assert_eq!(stats.duplicates_removed, 1);
        assert_eq!(cleaned.len(), 3);
        assert!(!cleaned[1].is_active());
    }

    /// A directory that duplicates a NON-ADJACENT same-named non-dir (separated
    /// by an entry that sorts before the dir's implicit trailing '/') must still
    /// be detected and the file dropped. upstream: flist.c:3052-3059 flist_find()
    /// as-regfile. This is the #145 half of the fix.
    #[test]
    fn flist_clean_dir_dups_nonadjacent_regfile() {
        // Sorted order: file "item" < file "item!" < dir "item" (= "item/").
        let mut entries = vec![make_file("item"), make_file("item!"), make_dir("item")];
        sort_file_list(&mut entries, false, false);
        assert_eq!(active_names(&entries), vec!["item", "item!", "item"]);
        let (cleaned, stats) = flist_clean(entries, false, false);
        assert_eq!(cleaned.len(), 3);
        assert_eq!(stats.duplicates_removed, 1);
        // The non-adjacent file "item" is tombstoned; "item!" and the dir survive.
        let survivors: Vec<(&str, bool)> = cleaned
            .iter()
            .filter(|e| e.is_active())
            .map(|e| (e.name(), e.is_dir()))
            .collect();
        assert_eq!(survivors, vec![("item!", false), ("item", true)]);
    }

    #[test]
    fn sort_and_clean_combined() {
        let entries = vec![
            make_file("z.txt"),
            make_dir("a"),
            make_file("a.txt"),
            make_file("a.txt"), // duplicate
        ];
        let (cleaned, stats) = sort_and_clean_file_list(entries, false, false, false, false);
        assert_eq!(stats.duplicates_removed, 1);
        // One slot tombstoned; length preserved.
        assert_eq!(cleaned.len(), 4);
        assert_eq!(active_names(&cleaned), vec!["a.txt", "z.txt", "a"]);
    }

    /// Comprehensive edge-case sort order golden test.
    ///
    /// Captures the exact expected ordering for tricky cases involving
    /// deep nesting, shared prefixes, file-vs-directory at same level,
    /// and implicit trailing slashes. Any change to the comparator that
    /// alters this ordering would break wire compatibility (NDX mismatch).
    #[test]
    fn sort_order_golden_comprehensive() {
        let mut entries = vec![
            // Root-level files
            make_file("a"),
            make_file("b.txt"),
            make_file("z"),
            // Root-level dirs
            make_dir("a"),
            make_dir("ab"),
            make_dir("b"),
            // Files that share prefix with dirs
            make_file("ab.txt"),
            make_file("a/file.txt"),
            make_file("a/z.txt"),
            // Nested dirs
            make_dir("a/sub"),
            make_file("a/sub/deep.txt"),
            // Dir with name that is prefix of another dir
            make_dir("a/sub/deep"),
            make_file("a/sub/deep/leaf.txt"),
            // Same-depth file vs dir disambiguation
            make_file("b/file.txt"),
            make_dir("b/dir"),
            make_file("b/dir/inner.txt"),
            // "." root marker
            make_dir("."),
            // Paths with shared multi-component prefixes
            make_dir("x/y"),
            make_file("x/y/a.txt"),
            make_file("x/y/b.txt"),
            make_dir("x/y/c"),
            make_file("x/y/c/d.txt"),
            make_file("x/z.txt"),
            make_dir("x"),
        ];

        sort_file_list(&mut entries, false, false);

        let names: Vec<&str> = entries.iter().map(|e| e.name()).collect();
        assert_eq!(
            names,
            vec![
                ".",
                // Root files before root dirs
                "a",
                "ab.txt",
                "b.txt",
                "z",
                // Root dir "a" and its contents
                "a",          // dir
                "a/file.txt", // file in a/
                "a/z.txt",    // file in a/
                "a/sub",      // dir in a/
                "a/sub/deep.txt",
                "a/sub/deep",          // dir
                "a/sub/deep/leaf.txt", // file in a/sub/deep/
                // Root dir "ab"
                "ab",
                // Root dir "b" and its contents
                "b",
                "b/file.txt",
                "b/dir",
                "b/dir/inner.txt",
                // Root dir "x" and its contents
                "x",
                "x/z.txt",
                "x/y",
                "x/y/a.txt",
                "x/y/b.txt",
                "x/y/c",
                "x/y/c/d.txt",
            ]
        );
    }

    /// Tests that a file named "foo" (non-dir) sorts before dir "foo" at
    /// the same level, because files sort before directories.
    #[test]
    fn file_before_same_name_dir() {
        let file = make_file("item");
        let dir = make_dir("item");
        assert_eq!(compare_file_entries(&file, &dir), Ordering::Less);
        assert_eq!(compare_file_entries(&dir, &file), Ordering::Greater);
    }

    /// Tests deeply nested paths with long shared prefixes.
    #[test]
    fn deep_nesting_shared_prefix() {
        let deep_file = make_file("a/b/c/d/e/f/g.txt");
        let deep_dir = make_dir("a/b/c/d/e/f/g");
        // File sorts before dir with same name
        assert_eq!(compare_file_entries(&deep_file, &deep_dir), Ordering::Less);

        let sibling_file = make_file("a/b/c/d/e/f/h.txt");
        // g.txt < h.txt alphabetically, both are files at same depth
        assert_eq!(
            compare_file_entries(&deep_file, &sibling_file),
            Ordering::Less
        );

        // Dir g/ sorts after file h.txt (dirs after files at same level)
        assert_eq!(
            compare_file_entries(&deep_dir, &sibling_file),
            Ordering::Greater
        );
    }

    /// Verifies that `use_qsort=true` produces the same ordering as the default
    /// stable sort. The comparison function is identical; only sort stability differs.
    #[test]
    fn qsort_flag_produces_correct_order() {
        let entries = vec![
            make_file("test.txt"),
            make_dir("subdir"),
            make_file("subdir/file.txt"),
            make_file("another.txt"),
            make_dir("."),
        ];

        let mut stable_entries = entries.clone();
        sort_file_list(&mut stable_entries, false, false);

        let mut qsort_entries = entries;
        sort_file_list(&mut qsort_entries, true, false);

        // With no duplicate keys, both algorithms must produce the same result
        let stable_names: Vec<&str> = stable_entries.iter().map(|e| e.name()).collect();
        let qsort_names: Vec<&str> = qsort_entries.iter().map(|e| e.name()).collect();
        assert_eq!(stable_names, qsort_names);
    }

    /// Verifies that `sort_and_clean_file_list` works with `use_qsort=true`.
    #[test]
    fn sort_and_clean_with_qsort() {
        let entries = vec![
            make_file("z.txt"),
            make_dir("a"),
            make_file("a.txt"),
            make_file("a.txt"), // duplicate
        ];
        let (cleaned, stats) = sort_and_clean_file_list(entries, true, false, false, false);
        assert_eq!(stats.duplicates_removed, 1);
        assert_eq!(cleaned.len(), 4);
        let names: Vec<&str> = cleaned
            .iter()
            .filter(|e| e.is_active())
            .map(|e| e.name())
            .collect();
        assert_eq!(names, vec!["a.txt", "z.txt", "a"]);
    }

    // Protocol < 29 sort tests

    /// Protocol < 29: directories do NOT sort after files at the same level.
    /// Plain lexicographic byte comparison, no implicit trailing '/'.
    /// upstream: flist.c:3223 - `t_path = t_ITEM` at protocol < 29.
    #[test]
    fn pre29_no_files_before_dirs() {
        let mut entries = vec![make_file("zebra.txt"), make_dir("aardvark")];
        sort_file_list(&mut entries, false, true);
        let names: Vec<&str> = entries.iter().map(|e| e.name()).collect();
        // At proto < 29: pure alphabetical, 'a' < 'z', so dir "aardvark" first
        assert_eq!(names, vec!["aardvark", "zebra.txt"]);
    }

    /// Protocol < 29: "." still comes first.
    #[test]
    fn pre29_dot_first() {
        let dot = make_dir(".");
        let file = make_file("abc.txt");
        let dot_bytes = dot.name_bytes();
        let file_bytes = file.name_bytes();
        assert_eq!(
            compare_with_keys_pre29(&dot_bytes, &file_bytes),
            Ordering::Less
        );
        assert_eq!(
            compare_with_keys_pre29(&file_bytes, &dot_bytes),
            Ordering::Greater
        );
    }

    /// Protocol < 29: mixed files and dirs sort in pure alphabetical order.
    #[test]
    fn pre29_sort_mixed_entries() {
        let mut entries = vec![
            make_file("test.txt"),
            make_dir("subdir"),
            make_file("subdir/file.txt"),
            make_file("another.txt"),
            make_dir("."),
        ];

        sort_file_list(&mut entries, false, true);

        let names: Vec<&str> = entries.iter().map(|e| e.name()).collect();
        // Proto < 29: "." first, then pure byte order
        assert_eq!(
            names,
            vec![".", "another.txt", "subdir", "subdir/file.txt", "test.txt"]
        );
    }

    /// Protocol < 29 golden test matching the comprehensive scenario.
    /// At proto < 29, dirs and files interleave alphabetically.
    #[test]
    fn pre29_sort_order_golden() {
        let mut entries = vec![
            make_file("a"),
            make_file("b.txt"),
            make_file("z"),
            make_dir("a"),
            make_dir("ab"),
            make_dir("b"),
            make_file("ab.txt"),
            make_file("a/file.txt"),
            make_dir("."),
        ];

        sort_file_list(&mut entries, false, true);

        let names: Vec<&str> = entries.iter().map(|e| e.name()).collect();
        // Pure lexicographic: "." first, then byte order.
        // File "a" and dir "a" have same name - stable sort keeps file first.
        assert_eq!(
            names,
            vec![
                ".",
                "a",
                "a",
                "a/file.txt",
                "ab",
                "ab.txt",
                "b",
                "b.txt",
                "z"
            ]
        );
    }
}
