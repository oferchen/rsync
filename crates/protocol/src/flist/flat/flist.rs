//! Arena-backed flat file list.
//!
//! [`FlatFileList`] is the top-level container for the flat backing store.
//! It owns a contiguous `Vec<FileEntryHeader>` for scalar metadata, a
//! [`PathArena`] for interned name/dirname strings, an [`ExtrasArena`] for
//! packed optional-field tails, and provides zero-copy views
//! ([`FlatFileEntry`]) that resolve path handles on the fly.
//!
//! [`Segment`] tracks INC_RECURSE sub-list boundaries within the unified
//! header array. Each segment records its `start_index` and `count` so
//! per-segment sorting, hardlink matching, and delete-pipeline publication
//! can operate on a contiguous slice without touching other segments.

use std::ops::Range;

use super::extras::{ExtrasArena, FlatExtras};
use super::header::FileEntryHeader;
use super::intern::PathArena;

/// INC_RECURSE sub-list boundary within a [`FlatFileList`].
///
/// Each segment tracks a contiguous range of headers corresponding to one
/// directory's file list. Segments are appended in reception order; the
/// header array grows monotonically and existing handles remain valid
/// (RSS-A.8.b design, finding F4).
///
/// Mirrors the `(flat_start, count)` tracking used by the legacy
/// `Vec<FileEntry>` path in both sender (`PendingSegment`) and receiver
/// (`ndx_segments`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Index of the first header in this segment within the owning
    /// [`FlatFileList`]'s header array.
    pub start_index: usize,
    /// Number of headers in this segment.
    pub count: usize,
}

/// Zero-copy view of a single file-list entry.
///
/// Borrows scalar metadata from the header array and resolves the interned
/// name and dirname handles through the [`PathArena`], yielding raw byte
/// slices with no allocation. The lifetime `'a` ties all borrows to the
/// owning [`FlatFileList`].
///
/// An optional reference to the [`ExtrasArena`] is carried so that trait
/// implementations (e.g. `FileEntryAccessor`) can decode extras fields on
/// demand without requiring a separate arena parameter at every call site.
pub struct FlatFileEntry<'a> {
    /// Reference to the fixed-size header for this entry.
    pub header: &'a FileEntryHeader,
    /// Resolved basename bytes from the path interner.
    pub name: &'a [u8],
    /// Resolved dirname bytes from the path interner.
    pub dirname: &'a [u8],
    /// Optional reference to the extras blob arena for decoding rarely-used
    /// metadata (symlink targets, device numbers, checksums, etc.).
    /// `None` until the extras arena is wired into [`FlatFileList`].
    pub extras_arena: Option<&'a ExtrasArena>,
}

/// Arena-backed flat file list.
///
/// Stores file-list entries as a contiguous array of [`FileEntryHeader`]
/// nodes plus a shared [`PathArena`] for deduplicated name/dirname strings
/// and an [`ExtrasArena`] for packed optional metadata (symlink targets,
/// device numbers, ACL/xattr indices, checksums, user/group names,
/// atime/crtime). This layout replaces the legacy `Vec<FileEntry>` with
/// pointer-chasing flexible-array tails, providing cache-friendly iteration
/// and O(1) indexed access with a smaller per-entry footprint.
pub struct FlatFileList {
    /// Contiguous array of fixed-size entry headers.
    headers: Vec<FileEntryHeader>,
    /// Shared string interner for name and dirname handles.
    paths: PathArena,
    /// Blob arena for packed extras tails referenced by each header's
    /// [`ExtrasRef`](super::header::ExtrasRef).
    extras: ExtrasArena,
    /// INC_RECURSE segment boundaries, in reception order.
    ///
    /// Empty when the file list is built as a single batch (non-incremental
    /// mode). Populated by [`extend_segment`](Self::extend_segment).
    segments: Vec<Segment>,
}

impl FlatFileList {
    /// Creates an empty flat file list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
            paths: PathArena::new(),
            extras: ExtrasArena::new(),
            segments: Vec::new(),
        }
    }

    /// Creates a flat file list pre-allocated for `cap` entries.
    ///
    /// Pre-sizes the header array and the path interner's span table.
    /// The path interner's byte arena and the extras arena grow on demand
    /// since per-entry payload sizes are not known up front.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            headers: Vec::with_capacity(cap),
            paths: PathArena::with_capacity(cap),
            extras: ExtrasArena::new(),
            segments: Vec::new(),
        }
    }

    /// Returns the number of entries in the list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.headers.len()
    }

    /// Returns `true` if the list contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    /// Returns a zero-copy view of the entry at `index`, or `None` if
    /// out of bounds.
    ///
    /// Resolves the entry's name and dirname handles through the path
    /// interner, producing borrowed byte slices with no allocation.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<FlatFileEntry<'_>> {
        let header = self.headers.get(index)?;
        let name = self.paths.resolve(header.name).as_bytes();
        let dirname = self.paths.resolve(header.dirname).as_bytes();
        Some(FlatFileEntry {
            header,
            name,
            dirname,
            extras_arena: None,
        })
    }

    /// Returns an iterator over zero-copy views of all entries.
    pub fn iter(&self) -> impl Iterator<Item = FlatFileEntry<'_>> {
        self.headers.iter().map(move |header| {
            let name = self.paths.resolve(header.name).as_bytes();
            let dirname = self.paths.resolve(header.dirname).as_bytes();
            FlatFileEntry {
                header,
                name,
                dirname,
                extras_arena: None,
            }
        })
    }

    /// Appends a header to the list.
    ///
    /// The caller must have already interned the entry's name and dirname
    /// strings into this list's [`PathArena`] (via `paths_mut`) and set
    /// the resulting handles on the header before calling `push`. The
    /// header's [`ExtrasRef`](super::header::ExtrasRef) should already
    /// reference a tail in this list's [`ExtrasArena`] (via
    /// [`push_with_extras`](Self::push_with_extras)) or be
    /// [`ExtrasRef::NO_EXTRAS`](super::header::ExtrasRef::NO_EXTRAS).
    pub fn push(&mut self, header: FileEntryHeader) {
        self.headers.push(header);
    }

    /// Encodes `extras` into the extras arena, sets the resulting
    /// [`ExtrasRef`](super::header::ExtrasRef) on `header`, and appends
    /// the header to the list.
    ///
    /// This is the primary builder entry point when the caller has optional
    /// metadata to attach. The caller must have already interned name and
    /// dirname via [`paths_mut`](Self::paths_mut).
    pub fn push_with_extras(&mut self, mut header: FileEntryHeader, extras: &FlatExtras) {
        header.extras = self.extras.append(extras);
        self.headers.push(header);
    }

    /// Sorts entries by dirname then name, using unsigned byte comparison.
    ///
    /// Uses `sort_unstable_by` since stability is not required for file-list
    /// ordering. The comparison resolves path handles through the interner
    /// and compares the resulting byte slices, matching upstream rsync's
    /// `f_name_cmp()` ordering (upstream: flist.c:3217).
    pub fn sort(&mut self) {
        let paths = &self.paths;
        self.headers.sort_unstable_by(|a, b| {
            let a_dir = paths.resolve(a.dirname).as_bytes();
            let b_dir = paths.resolve(b.dirname).as_bytes();
            let a_name = paths.resolve(a.name).as_bytes();
            let b_name = paths.resolve(b.name).as_bytes();
            a_dir.cmp(b_dir).then_with(|| a_name.cmp(b_name))
        });
    }

    /// Returns a shared reference to the path interner.
    #[must_use]
    pub fn paths(&self) -> &PathArena {
        &self.paths
    }

    /// Returns a mutable reference to the path interner.
    ///
    /// Used by builders to intern name and dirname strings before pushing
    /// the corresponding header.
    pub fn paths_mut(&mut self) -> &mut PathArena {
        &mut self.paths
    }

    /// Returns a shared reference to the extras arena.
    #[must_use]
    pub fn extras(&self) -> &ExtrasArena {
        &self.extras
    }

    /// Returns a mutable reference to the extras arena.
    ///
    /// Used by builders that encode extras separately before pushing a
    /// header whose [`ExtrasRef`](super::header::ExtrasRef) already
    /// references a tail in this arena.
    pub fn extras_mut(&mut self) -> &mut ExtrasArena {
        &mut self.extras
    }

    // -----------------------------------------------------------------
    // INC_RECURSE segment tracking
    // -----------------------------------------------------------------

    /// Appends a batch of headers as a new INC_RECURSE segment.
    ///
    /// Records a [`Segment`] boundary at the current end of the header
    /// array, then pushes all `headers` in order. Path handles and extras
    /// refs carried by the headers must already reference this list's
    /// [`PathArena`] and [`ExtrasArena`] (interned via `paths_mut` and
    /// `extras_mut` or `push_with_extras` before calling this method).
    ///
    /// Existing `PathHandle` and `ExtrasRef` values from prior segments
    /// remain valid because all three backing stores are append-only and
    /// all references are index/offset-based (RSS-A.8.b design, finding
    /// F4).
    ///
    /// Returns the zero-based segment index of the newly created segment.
    pub fn extend_segment(&mut self, headers: &[FileEntryHeader]) -> usize {
        let start_index = self.headers.len();
        let count = headers.len();
        self.headers.extend_from_slice(headers);
        let seg_idx = self.segments.len();
        self.segments.push(Segment { start_index, count });
        seg_idx
    }

    /// Returns the number of tracked segments.
    ///
    /// Zero when the list was built as a single batch without
    /// [`extend_segment`](Self::extend_segment).
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Returns the [`Segment`] at `index`, or `None` if out of bounds.
    #[must_use]
    pub fn segment(&self, index: usize) -> Option<Segment> {
        self.segments.get(index).copied()
    }

    /// Returns the header-array index range for segment `index`.
    ///
    /// Returns `None` if `index` is out of bounds.
    #[must_use]
    pub fn segment_range(&self, index: usize) -> Option<Range<usize>> {
        self.segments
            .get(index)
            .map(|s| s.start_index..s.start_index + s.count)
    }

    /// Returns a slice of all tracked segments.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Sorts only the headers within segment `index` by dirname-then-name.
    ///
    /// Headers outside the segment are untouched. Uses the same unsigned
    /// byte comparison as [`sort`](Self::sort), matching upstream rsync's
    /// `f_name_cmp()` ordering (upstream: flist.c:3217).
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds.
    pub fn sort_segment(&mut self, index: usize) {
        let seg = self.segments[index];
        let range = seg.start_index..seg.start_index + seg.count;
        self.sort_range(range);
    }

    /// Sorts a sub-range of the header array by dirname-then-name.
    ///
    /// Used by [`sort_segment`](Self::sort_segment) and available for
    /// callers that track segment boundaries externally (e.g. the
    /// receiver's `ndx_segments` table).
    ///
    /// # Panics
    ///
    /// Panics if `range` is out of bounds for the header array.
    pub fn sort_range(&mut self, range: Range<usize>) {
        let paths = &self.paths;
        self.headers[range].sort_unstable_by(|a, b| {
            let a_dir = paths.resolve(a.dirname).as_bytes();
            let b_dir = paths.resolve(b.dirname).as_bytes();
            let a_name = paths.resolve(a.name).as_bytes();
            let b_name = paths.resolve(b.name).as_bytes();
            a_dir.cmp(b_dir).then_with(|| a_name.cmp(b_name))
        });
    }

    /// Returns a slice of headers for the given range.
    ///
    /// Useful for segment-scoped operations (hardlink matching, delete
    /// pipeline publication, iteration).
    ///
    /// # Panics
    ///
    /// Panics if `range` is out of bounds for the header array.
    #[must_use]
    pub fn headers_slice(&self, range: Range<usize>) -> &[FileEntryHeader] {
        &self.headers[range]
    }
}

impl Default for FlatFileList {
    fn default() -> Self {
        Self::new()
    }
}
