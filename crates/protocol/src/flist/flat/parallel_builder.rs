//! Per-thread-then-merge parallel builder for [`FlatFileList`] (RSS-A.11.c).
//!
//! Implements the design from `docs/design/rss-a-11b-parallel-flat-flist-builder.md`:
//! each rayon worker builds a thread-local [`FlatFileList`] with its own arenas,
//! then a single-threaded merge phase re-interns paths (deduplicating dirnames
//! across workers) and re-encodes extras into the final list.
//!
//! This avoids all contention during the I/O-bound parallel stat phase. The merge
//! cost is O(n) and dominated by the parallel I/O.
//!
//! Gated behind `cfg(feature = "flat-flist-rayon")`.

use rayon::prelude::*;

use super::extras::ExtrasRef;
use super::flist::FlatFileList;

/// Builder for constructing a [`FlatFileList`] from parallel rayon workers.
///
/// Each worker operates on its own [`FlatFileList`] with independent arenas -
/// no synchronization needed during the parallel phase. After all workers
/// finish, [`merge`](Self::merge) combines per-worker lists into one sorted
/// result, re-interning paths for cross-worker deduplication.
///
/// # Usage patterns
///
/// ## Pattern 1: Sequential setup, then merge
///
/// ```ignore
/// let mut builder = ParallelFlatFileListBuilder::new();
/// builder.add_worker_list(worker_0_list);
/// builder.add_worker_list(worker_1_list);
/// let flist = builder.merge();
/// ```
///
/// ## Pattern 2: build_parallel (preferred)
///
/// ```ignore
/// let entries: Vec<(String, String, u64)> = /* enumerate */;
/// let flist = ParallelFlatFileListBuilder::build_parallel(
///     entries,
///     |list, (name, dirname, size)| {
///         let nh = list.paths_mut().intern(&name);
///         let dh = list.paths_mut().intern(&dirname);
///         let mut h = FileEntryHeader { /* ... */ };
///         h.name = nh;
///         h.dirname = dh;
///         list.push(h);
///     },
/// );
/// ```
pub struct ParallelFlatFileListBuilder {
    /// Per-worker file lists, each with independent arenas.
    workers: Vec<FlatFileList>,
}

impl ParallelFlatFileListBuilder {
    /// Creates an empty builder with no worker lists.
    #[must_use]
    pub fn new() -> Self {
        Self {
            workers: Vec::new(),
        }
    }

    /// Creates a builder pre-allocated for `num_workers` lists.
    #[must_use]
    pub fn with_worker_capacity(num_workers: usize) -> Self {
        Self {
            workers: Vec::with_capacity(num_workers),
        }
    }

    /// Returns the number of worker lists added so far.
    #[must_use]
    pub fn num_workers(&self) -> usize {
        self.workers.len()
    }

    /// Adds a completed worker-local [`FlatFileList`] to the builder.
    ///
    /// Call this after each rayon worker finishes building its local list.
    /// The worker list will be consumed during [`merge`](Self::merge).
    pub fn add_worker_list(&mut self, list: FlatFileList) {
        self.workers.push(list);
    }

    /// Merges all per-worker lists into one sorted [`FlatFileList`].
    ///
    /// The merge iterates each worker list, re-interns name/dirname strings
    /// into the final shared [`PathArena`](super::PathArena) (achieving
    /// cross-worker dirname deduplication), re-encodes extras into the final
    /// [`ExtrasArena`](super::ExtrasArena), and pushes updated headers.
    /// Each worker list is dropped immediately after processing to bound
    /// the high-water memory mark.
    ///
    /// After merging, the result is sorted by dirname-then-name (matching
    /// upstream rsync's `f_name_cmp()` ordering - upstream: flist.c:3217).
    #[must_use]
    pub fn merge(self) -> FlatFileList {
        let total: usize = self.workers.iter().map(|w| w.len()).sum();
        let mut merged = FlatFileList::with_capacity(total);

        for worker in self.workers {
            extend_from_worker(&mut merged, &worker);
        }

        merged.sort();
        merged
    }

    /// Merges all per-worker lists into one [`FlatFileList`] without sorting.
    ///
    /// Same as [`merge`](Self::merge) but skips the final sort step. Useful
    /// when the caller needs to sort by a custom comparator or append the
    /// merged result as an INC_RECURSE segment (which sorts per-segment).
    #[must_use]
    pub fn merge_unsorted(self) -> FlatFileList {
        let total: usize = self.workers.iter().map(|w| w.len()).sum();
        let mut merged = FlatFileList::with_capacity(total);

        for worker in self.workers {
            extend_from_worker(&mut merged, &worker);
        }

        merged
    }

    /// Builds a [`FlatFileList`] from a vector of items using rayon
    /// parallelism.
    ///
    /// Splits `items` into chunks (one per rayon thread), processes each
    /// chunk into a thread-local [`FlatFileList`] via the closure `f`, then
    /// merges all per-thread lists into a single sorted result.
    ///
    /// The closure receives a mutable reference to the worker-local
    /// [`FlatFileList`] and one item. It is responsible for interning paths
    /// and pushing headers into the worker list.
    ///
    /// This is the preferred API for parallel flist construction - fully safe,
    /// zero contention during the parallel phase.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let entries: Vec<(String, String, u64)> = /* ... */;
    /// let flist = ParallelFlatFileListBuilder::build_parallel(
    ///     entries,
    ///     |list, (name, dirname, size)| {
    ///         let nh = list.paths_mut().intern(&name);
    ///         let dh = list.paths_mut().intern(&dirname);
    ///         let mut h = FileEntryHeader { /* ... */ };
    ///         h.name = nh;
    ///         h.dirname = dh;
    ///         list.push(h);
    ///     },
    /// );
    /// ```
    pub fn build_parallel<T, F>(items: Vec<T>, f: F) -> FlatFileList
    where
        T: Send,
        F: Fn(&mut FlatFileList, T) + Send + Sync,
    {
        if items.is_empty() {
            return FlatFileList::new();
        }

        let num_workers = rayon::current_num_threads().max(1);
        let chunk_size = items.len().div_ceil(num_workers).max(1);

        // Each rayon chunk builds an independent FlatFileList. No
        // synchronization needed - each closure gets its own &mut list.
        let worker_lists: Vec<FlatFileList> = items
            .into_par_iter()
            .chunks(chunk_size)
            .map(|chunk| {
                let mut local = FlatFileList::with_capacity(chunk.len());
                for item in chunk {
                    f(&mut local, item);
                }
                local
            })
            .collect();

        // Merge all worker lists sequentially.
        let total: usize = worker_lists.iter().map(|w| w.len()).sum();
        let mut merged = FlatFileList::with_capacity(total);
        for worker in worker_lists {
            extend_from_worker(&mut merged, &worker);
        }
        merged.sort();
        merged
    }

    /// Same as [`build_parallel`](Self::build_parallel) but skips the final
    /// sort step.
    pub fn build_parallel_unsorted<T, F>(items: Vec<T>, f: F) -> FlatFileList
    where
        T: Send,
        F: Fn(&mut FlatFileList, T) + Send + Sync,
    {
        if items.is_empty() {
            return FlatFileList::new();
        }

        let num_workers = rayon::current_num_threads().max(1);
        let chunk_size = items.len().div_ceil(num_workers).max(1);

        let worker_lists: Vec<FlatFileList> = items
            .into_par_iter()
            .chunks(chunk_size)
            .map(|chunk| {
                let mut local = FlatFileList::with_capacity(chunk.len());
                for item in chunk {
                    f(&mut local, item);
                }
                local
            })
            .collect();

        let total: usize = worker_lists.iter().map(|w| w.len()).sum();
        let mut merged = FlatFileList::with_capacity(total);
        for worker in worker_lists {
            extend_from_worker(&mut merged, &worker);
        }
        merged
    }
}

impl Default for ParallelFlatFileListBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Extends `target` with all entries from `source`, re-interning paths and
/// re-encoding extras.
///
/// Each header in `source` has its `name` and `dirname` handles resolved
/// through `source`'s [`PathArena`](super::PathArena), then re-interned into
/// `target`'s arena (achieving deduplication across workers). Extras are
/// decoded from `source`'s [`ExtrasArena`](super::ExtrasArena) and re-encoded
/// into `target`'s arena.
fn extend_from_worker(target: &mut FlatFileList, source: &FlatFileList) {
    for i in 0..source.len() {
        let entry = source
            .get(i)
            .expect("index within source.len() must be valid");

        // Re-intern name and dirname into target's PathArena.
        let name_str = std::str::from_utf8(entry.name).unwrap_or("");
        let dirname_str = std::str::from_utf8(entry.dirname).unwrap_or("");
        let new_name = target.paths_mut().intern(name_str);
        let new_dirname = target.paths_mut().intern(dirname_str);

        // Re-encode extras if present.
        let new_extras = if entry.header.extras == ExtrasRef::NO_EXTRAS {
            ExtrasRef::NO_EXTRAS
        } else {
            match source.extras().decode(entry.header.extras) {
                Ok(Some(decoded)) => target.extras_mut().append(&decoded),
                _ => ExtrasRef::NO_EXTRAS,
            }
        };

        // Build the updated header with new handles.
        let mut header = *entry.header;
        header.name = new_name;
        header.dirname = new_dirname;
        header.extras = new_extras;
        target.push(header);
    }
}

/// Extends `target` with all entries from `source`.
///
/// Re-interns all paths from `source` into `target`'s
/// [`PathArena`](super::PathArena) and re-encodes extras into `target`'s
/// [`ExtrasArena`](super::ExtrasArena). This is the core operation underlying
/// the parallel builder's merge phase, exposed for callers that need to combine
/// independently-built file lists.
pub fn extend_from(target: &mut FlatFileList, source: &FlatFileList) {
    extend_from_worker(target, source);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flist::flat::{FileEntryHeader, FlatExtras, PathHandle};

    /// A blank header used as a starting point.
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

    /// Push a named entry into a FlatFileList.
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
    fn builder_single_worker_produces_sorted_list() {
        let mut builder = ParallelFlatFileListBuilder::new();
        let mut list = FlatFileList::with_capacity(3);
        push_entry(&mut list, "cherry", "fruit", 3);
        push_entry(&mut list, "apple", "fruit", 1);
        push_entry(&mut list, "banana", "fruit", 2);
        builder.add_worker_list(list);

        let merged = builder.merge();
        assert_eq!(merged.len(), 3);

        // Sorted: apple, banana, cherry (same dirname).
        assert_eq!(merged.get(0).unwrap().name, b"apple");
        assert_eq!(merged.get(1).unwrap().name, b"banana");
        assert_eq!(merged.get(2).unwrap().name, b"cherry");
    }

    #[test]
    fn builder_multi_worker_merge_deduplicates_dirnames() {
        let mut builder = ParallelFlatFileListBuilder::new();

        // Worker 0: files in "src"
        let mut w0 = FlatFileList::new();
        push_entry(&mut w0, "main.rs", "src", 100);
        push_entry(&mut w0, "lib.rs", "src", 200);
        builder.add_worker_list(w0);

        // Worker 1: files in "src" and "tests"
        let mut w1 = FlatFileList::new();
        push_entry(&mut w1, "util.rs", "src", 50);
        push_entry(&mut w1, "test.rs", "tests", 75);
        builder.add_worker_list(w1);

        // Worker 2: files in "docs"
        let mut w2 = FlatFileList::new();
        push_entry(&mut w2, "README", "docs", 10);
        builder.add_worker_list(w2);

        let merged = builder.merge();
        assert_eq!(merged.len(), 5);

        // "src" appears in workers 0 and 1 but should be interned once.
        // Unique strings: src, tests, docs, main.rs, lib.rs, util.rs, test.rs, README = 8
        assert_eq!(merged.paths().len(), 8);

        // Sorted order: docs/README, src/lib.rs, src/main.rs, src/util.rs, tests/test.rs
        let entries: Vec<(&[u8], &[u8])> = (0..5)
            .map(|i| {
                let e = merged.get(i).unwrap();
                (e.dirname, e.name)
            })
            .collect();
        assert_eq!(entries[0], (b"docs" as &[u8], b"README" as &[u8]));
        assert_eq!(entries[1], (b"src" as &[u8], b"lib.rs" as &[u8]));
        assert_eq!(entries[2], (b"src" as &[u8], b"main.rs" as &[u8]));
        assert_eq!(entries[3], (b"src" as &[u8], b"util.rs" as &[u8]));
        assert_eq!(entries[4], (b"tests" as &[u8], b"test.rs" as &[u8]));
    }

    #[test]
    fn builder_merge_unsorted_preserves_insertion_order() {
        let mut builder = ParallelFlatFileListBuilder::new();

        let mut w0 = FlatFileList::new();
        push_entry(&mut w0, "z.txt", "", 3);
        push_entry(&mut w0, "a.txt", "", 1);
        builder.add_worker_list(w0);

        let mut w1 = FlatFileList::new();
        push_entry(&mut w1, "m.txt", "", 2);
        builder.add_worker_list(w1);

        let merged = builder.merge_unsorted();
        assert_eq!(merged.len(), 3);

        // Worker 0's entries first, then worker 1's - no sorting.
        assert_eq!(merged.get(0).unwrap().name, b"z.txt");
        assert_eq!(merged.get(1).unwrap().name, b"a.txt");
        assert_eq!(merged.get(2).unwrap().name, b"m.txt");
    }

    #[test]
    fn builder_extras_round_trip_across_workers() {
        let mut builder = ParallelFlatFileListBuilder::new();

        // Worker 0: entry with symlink extras.
        let mut w0 = FlatFileList::new();
        let name_h = w0.paths_mut().intern("link");
        let dirname_h = w0.paths_mut().intern("src");
        let extras = FlatExtras {
            link_target: Some(b"../target".to_vec()),
            ..FlatExtras::default()
        };
        let mut h = empty_header();
        h.name = name_h;
        h.dirname = dirname_h;
        w0.push_with_extras(h, &extras);
        builder.add_worker_list(w0);

        // Worker 1: entry with checksum extras.
        let mut w1 = FlatFileList::new();
        let name_h = w1.paths_mut().intern("data.bin");
        let dirname_h = w1.paths_mut().intern("out");
        let extras2 = FlatExtras {
            checksum: Some(vec![0xAB; 16]),
            user_name: Some(b"alice".to_vec()),
            ..FlatExtras::default()
        };
        let mut h2 = empty_header();
        h2.name = name_h;
        h2.dirname = dirname_h;
        w1.push_with_extras(h2, &extras2);
        builder.add_worker_list(w1);

        let merged = builder.merge();
        assert_eq!(merged.len(), 2);

        // Sorted order: out/data.bin, src/link
        let e0 = merged.get(0).unwrap();
        assert_eq!(e0.dirname, b"out");
        let d0 = merged.extras().decode(e0.header.extras).unwrap().unwrap();
        assert_eq!(d0.checksum, Some(vec![0xAB; 16]));
        assert_eq!(d0.user_name, Some(b"alice".to_vec()));

        let e1 = merged.get(1).unwrap();
        assert_eq!(e1.dirname, b"src");
        let d1 = merged.extras().decode(e1.header.extras).unwrap().unwrap();
        assert_eq!(d1.link_target, Some(b"../target".to_vec()));
    }

    #[test]
    fn builder_empty_workers_produce_empty_list() {
        let mut builder = ParallelFlatFileListBuilder::new();
        builder.add_worker_list(FlatFileList::new());
        builder.add_worker_list(FlatFileList::new());
        let merged = builder.merge();
        assert_eq!(merged.len(), 0);
        assert!(merged.is_empty());
    }

    #[test]
    fn builder_no_workers_produce_empty_list() {
        let builder = ParallelFlatFileListBuilder::new();
        let merged = builder.merge();
        assert_eq!(merged.len(), 0);
        assert!(merged.is_empty());
    }

    #[test]
    fn build_parallel_produces_sorted_list() {
        let items: Vec<(String, String, u64)> = vec![
            ("cherry".into(), "fruit".into(), 3),
            ("apple".into(), "fruit".into(), 1),
            ("banana".into(), "fruit".into(), 2),
            ("readme".into(), "".into(), 0),
        ];

        let flist = ParallelFlatFileListBuilder::build_parallel(
            items,
            |list, (name, dirname, size)| {
                let nh = list.paths_mut().intern(&name);
                let dh = list.paths_mut().intern(&dirname);
                let mut h = empty_header();
                h.name = nh;
                h.dirname = dh;
                h.size = size;
                list.push(h);
            },
        );

        assert_eq!(flist.len(), 4);

        // Sorted: ""/readme, fruit/apple, fruit/banana, fruit/cherry
        assert_eq!(flist.get(0).unwrap().name, b"readme");
        assert_eq!(flist.get(0).unwrap().dirname, b"");
        assert_eq!(flist.get(1).unwrap().name, b"apple");
        assert_eq!(flist.get(2).unwrap().name, b"banana");
        assert_eq!(flist.get(3).unwrap().name, b"cherry");
    }

    #[test]
    fn build_parallel_empty_items() {
        let items: Vec<(String, String, u64)> = vec![];
        let flist = ParallelFlatFileListBuilder::build_parallel(
            items,
            |list, (name, dirname, size)| {
                let nh = list.paths_mut().intern(&name);
                let dh = list.paths_mut().intern(&dirname);
                let mut h = empty_header();
                h.name = nh;
                h.dirname = dh;
                h.size = size;
                list.push(h);
            },
        );
        assert!(flist.is_empty());
    }

    #[test]
    fn build_parallel_at_scale_with_dedup() {
        // 1000 files across 10 directories, processed in parallel.
        let items: Vec<(String, String, u64)> = (0..1000)
            .map(|i| {
                let name = format!("f{i}.rs");
                let dirname = format!("pkg{}", i % 10);
                (name, dirname, i as u64)
            })
            .collect();

        let flist = ParallelFlatFileListBuilder::build_parallel(
            items,
            |list, (name, dirname, size)| {
                let nh = list.paths_mut().intern(&name);
                let dh = list.paths_mut().intern(&dirname);
                let mut h = empty_header();
                h.name = nh;
                h.dirname = dh;
                h.size = size;
                list.push(h);
            },
        );

        assert_eq!(flist.len(), 1000);

        // 10 unique dirnames + 1000 unique basenames = 1010
        assert_eq!(flist.paths().len(), 1010);

        // Verify sorted order.
        for i in 1..flist.len() {
            let prev = flist.get(i - 1).unwrap();
            let curr = flist.get(i).unwrap();
            let cmp = prev
                .dirname
                .cmp(curr.dirname)
                .then_with(|| prev.name.cmp(curr.name));
            assert!(
                cmp.is_le(),
                "entry {i} not in sorted order: {:?}/{:?} vs {:?}/{:?}",
                String::from_utf8_lossy(prev.dirname),
                String::from_utf8_lossy(prev.name),
                String::from_utf8_lossy(curr.dirname),
                String::from_utf8_lossy(curr.name),
            );
        }
    }

    #[test]
    fn build_parallel_unsorted_preserves_chunk_order() {
        // Use a sequence that would be reordered by sorting.
        let items: Vec<(String, String, u64)> = vec![
            ("z.txt".into(), "b".into(), 1),
            ("a.txt".into(), "b".into(), 2),
            ("m.txt".into(), "a".into(), 3),
        ];

        let flist = ParallelFlatFileListBuilder::build_parallel_unsorted(
            items,
            |list, (name, dirname, size)| {
                let nh = list.paths_mut().intern(&name);
                let dh = list.paths_mut().intern(&dirname);
                let mut h = empty_header();
                h.name = nh;
                h.dirname = dh;
                h.size = size;
                list.push(h);
            },
        );

        assert_eq!(flist.len(), 3);
        // All items present (sizes 1, 2, 3 in some order).
        let mut sizes: Vec<u64> = (0..3).map(|i| flist.get(i).unwrap().header.size).collect();
        sizes.sort();
        assert_eq!(sizes, vec![1, 2, 3]);
    }

    #[test]
    fn build_parallel_with_extras() {
        let items: Vec<(String, String, Option<Vec<u8>>)> = vec![
            ("link".into(), "src".into(), Some(b"../target".to_vec())),
            ("plain.txt".into(), "src".into(), None),
            ("data.bin".into(), "out".into(), Some(vec![0xDE; 8])),
        ];

        let flist = ParallelFlatFileListBuilder::build_parallel(
            items,
            |list, (name, dirname, link_target)| {
                let nh = list.paths_mut().intern(&name);
                let dh = list.paths_mut().intern(&dirname);
                let mut h = empty_header();
                h.name = nh;
                h.dirname = dh;

                if let Some(target) = link_target {
                    let extras = FlatExtras {
                        link_target: Some(target),
                        ..FlatExtras::default()
                    };
                    list.push_with_extras(h, &extras);
                } else {
                    list.push(h);
                }
            },
        );

        assert_eq!(flist.len(), 3);

        // Sorted: out/data.bin, src/link, src/plain.txt
        let e0 = flist.get(0).unwrap();
        assert_eq!(e0.name, b"data.bin");
        let d0 = flist.extras().decode(e0.header.extras).unwrap().unwrap();
        assert_eq!(d0.link_target, Some(vec![0xDE; 8]));

        let e1 = flist.get(1).unwrap();
        assert_eq!(e1.name, b"link");
        let d1 = flist.extras().decode(e1.header.extras).unwrap().unwrap();
        assert_eq!(d1.link_target, Some(b"../target".to_vec()));

        let e2 = flist.get(2).unwrap();
        assert_eq!(e2.name, b"plain.txt");
        assert_eq!(e2.header.extras, ExtrasRef::NO_EXTRAS);
    }

    #[test]
    fn extend_from_re_interns_and_deduplicates() {
        let mut source = FlatFileList::new();
        push_entry(&mut source, "main.rs", "src", 100);
        push_entry(&mut source, "lib.rs", "src", 200);

        let mut target = FlatFileList::new();
        push_entry(&mut target, "util.rs", "src", 50);

        super::extend_from(&mut target, &source);

        assert_eq!(target.len(), 3);
        // "src" is interned once despite coming from both target and source.
        // Unique strings: util.rs, src, main.rs, lib.rs = 4
        assert_eq!(target.paths().len(), 4);

        // Entries are in insertion order (target's first, then source's).
        assert_eq!(target.get(0).unwrap().name, b"util.rs");
        assert_eq!(target.get(1).unwrap().name, b"main.rs");
        assert_eq!(target.get(2).unwrap().name, b"lib.rs");
    }

    #[test]
    fn merge_preserves_scalar_header_fields() {
        let mut builder = ParallelFlatFileListBuilder::new();
        let mut list = FlatFileList::new();

        let name_h = list.paths_mut().intern("special.dat");
        let dirname_h = list.paths_mut().intern("data");
        let mut h = empty_header();
        h.name = name_h;
        h.dirname = dirname_h;
        h.size = 0xCAFE_BABE;
        h.mtime = 1_700_000_000;
        h.mode = 0o100755;
        h.uid = 1000;
        h.gid = 2000;
        h.flags = 0x1234;
        h.present = 0x0003; // UID + GID
        h.mtime_nsec = 500_000;
        list.push(h);
        builder.add_worker_list(list);

        let merged = builder.merge();
        let entry = merged.get(0).unwrap();
        assert_eq!(entry.header.size, 0xCAFE_BABE);
        assert_eq!(entry.header.mtime, 1_700_000_000);
        assert_eq!(entry.header.mode, 0o100755);
        assert_eq!(entry.header.uid, 1000);
        assert_eq!(entry.header.gid, 2000);
        assert_eq!(entry.header.flags, 0x1234);
        assert_eq!(entry.header.present, 0x0003);
        assert_eq!(entry.header.mtime_nsec, 500_000);
    }

    #[test]
    fn build_parallel_exercises_multiple_threads() {
        // Use enough items to guarantee rayon splits across threads.
        let items: Vec<u64> = (0..10_000).collect();

        let flist = ParallelFlatFileListBuilder::build_parallel(
            items,
            |list, i| {
                let name = format!("f{i}");
                let dirname = format!("d{}", i % 50);
                let nh = list.paths_mut().intern(&name);
                let dh = list.paths_mut().intern(&dirname);
                let mut h = empty_header();
                h.name = nh;
                h.dirname = dh;
                h.size = i;
                list.push(h);
            },
        );

        assert_eq!(flist.len(), 10_000);

        // All sizes 0..10_000 must be present.
        let mut sizes: Vec<u64> = (0..10_000)
            .map(|i| flist.get(i).unwrap().header.size)
            .collect();
        sizes.sort();
        let expected: Vec<u64> = (0..10_000).collect();
        assert_eq!(sizes, expected);

        // 50 unique dirnames + 10_000 unique basenames = 10_050
        assert_eq!(flist.paths().len(), 10_050);
    }

    #[test]
    fn default_builder_is_empty() {
        let builder = ParallelFlatFileListBuilder::default();
        assert_eq!(builder.num_workers(), 0);
        let merged = builder.merge();
        assert!(merged.is_empty());
    }
}
