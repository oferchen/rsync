//! Name/dirname string interner backing the flat store's [`PathHandle`].
//!
//! See `docs/design/flat-flist-representation.md` (RSS-A.5.c and the
//! "Name and dirname interning vs upstream's sharing" section of RSS-A.5.f)
//! for the design this implements.
//!
//! Every unique name or dirname string is stored exactly once in a
//! contiguous byte arena; a 4-byte [`PathHandle`] indexes a side table of
//! `(offset, len)` spans into that arena. Interning the same string twice
//! returns the same handle, so the store gets both the dirname sharing
//! upstream's `lastdir` cache provides (upstream: `flist.c:765-773`) and the
//! basename deduplication upstream lacks - upstream stores each basename
//! inline in the `file_struct` flexible array tail (upstream: `rsync.h:808`),
//! duplicating two files named `README` in different directories. Resolving a
//! handle is an O(1) indexed read: one table lookup plus a slice of the
//! arena, with no hash lookup (mirroring the design's "indexed read once
//! frozen" contract without needing a separate freeze step).
//!
//! The empty string is never stored; it interns to and resolves through
//! [`PathHandle::NONE`], matching the header's null sentinel for absent
//! name/dirname slots.

use std::collections::HashMap;

use super::header::PathHandle;

/// Interns name/dirname strings into a compact arena keyed by [`PathHandle`].
///
/// The interner owns a single growable byte arena holding each unique string
/// back to back, plus a `spans` table mapping a handle's index to the
/// `(offset, len)` of its bytes and a `dedup` map for O(1) deduplication.
/// Handles are dense, zero-based `u32` indices assigned in first-seen order;
/// [`PathHandle::NONE`] (`u32::MAX`) is reserved as the empty/absent
/// sentinel and is never assigned to a stored string.
///
/// # Examples
///
/// ```ignore
/// // Requires the `flat-flist` feature.
/// use protocol::flist::{PathArena, PathHandle};
///
/// let mut arena = PathArena::new();
/// let a = arena.intern("README");
/// let b = arena.intern("README");
/// // Identical strings share one handle and one arena copy.
/// assert_eq!(a, b);
/// assert_eq!(arena.resolve(a), "README");
///
/// // The empty string is the NONE sentinel, never stored.
/// assert_eq!(arena.intern(""), PathHandle::NONE);
/// assert_eq!(arena.resolve(PathHandle::NONE), "");
/// assert_eq!(arena.len(), 1);
/// ```
#[derive(Debug, Default)]
pub struct PathArena {
    /// Contiguous bytes of every interned string, stored once each.
    bytes: Vec<u8>,
    /// Per-handle `(offset, len)` spans into `bytes`, indexed by handle value.
    spans: Vec<(u32, u32)>,
    /// Maps a string to its already-assigned handle for O(1) dedup.
    dedup: HashMap<Box<str>, PathHandle>,
}

impl PathArena {
    /// Creates an empty interner with no allocated arena.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an interner pre-sized for `capacity` distinct strings.
    ///
    /// Reserves the span table and dedup map; the byte arena grows on
    /// demand since per-string lengths are not known up front.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::new(),
            spans: Vec::with_capacity(capacity),
            dedup: HashMap::with_capacity(capacity),
        }
    }

    /// Interns `s`, returning a handle that resolves back to it.
    ///
    /// Returns [`PathHandle::NONE`] for the empty string without storing
    /// anything. For a non-empty string, returns the existing handle if `s`
    /// was interned before (pointer-stable dedup), otherwise appends the
    /// bytes to the arena, records a span, and assigns the next handle.
    ///
    /// # Panics
    ///
    /// Panics if more than `u32::MAX` distinct non-empty strings are
    /// interned, since handles are 4-byte indices. The flat store is sized
    /// for file lists far below this bound.
    pub fn intern(&mut self, s: &str) -> PathHandle {
        if s.is_empty() {
            return PathHandle::NONE;
        }
        if let Some(&handle) = self.dedup.get(s) {
            return handle;
        }

        let index = u32::try_from(self.spans.len())
            .expect("PathArena exceeded u32::MAX distinct interned strings");
        // u32::MAX is the NONE sentinel and must never name a real string.
        assert!(index != PathHandle::NONE.0, "PathArena handle space exhausted");

        let offset = u32::try_from(self.bytes.len()).expect("PathArena byte arena exceeded 4 GiB");
        let len = u32::try_from(s.len()).expect("interned string exceeds u32::MAX bytes");
        self.bytes.extend_from_slice(s.as_bytes());
        self.spans.push((offset, len));

        let handle = PathHandle(index);
        self.dedup.insert(Box::from(s), handle);
        handle
    }

    /// Resolves `handle` to its interned string slice.
    ///
    /// Returns `""` for [`PathHandle::NONE`] (the empty/absent sentinel).
    ///
    /// # Panics
    ///
    /// Panics if `handle` was not produced by this interner (out-of-range
    /// index). Handles are only valid for the interner that issued them.
    #[must_use]
    pub fn resolve(&self, handle: PathHandle) -> &str {
        match self.get(handle) {
            Some(s) => s,
            None => panic!("PathHandle({}) is not valid for this PathArena", handle.0),
        }
    }

    /// Resolves `handle` to its interned string, or `None` if absent.
    ///
    /// Returns `None` for [`PathHandle::NONE`] and for any handle whose index
    /// is out of range for this interner. Non-sentinel valid handles always
    /// resolve to a stored string.
    #[must_use]
    pub fn get(&self, handle: PathHandle) -> Option<&str> {
        if handle == PathHandle::NONE {
            return None;
        }
        let (offset, len) = *self.spans.get(handle.0 as usize)?;
        let (start, end) = (offset as usize, offset as usize + len as usize);
        // Bytes are only ever appended from valid &str, so this slice is UTF-8.
        std::str::from_utf8(&self.bytes[start..end]).ok()
    }

    /// Returns the number of distinct non-empty strings interned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Returns `true` if no non-empty strings have been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Returns the total bytes held in the string arena.
    ///
    /// This is the deduplicated footprint: each unique string contributes
    /// its length once, regardless of how many entries reference it.
    #[must_use]
    pub fn bytes_len(&self) -> usize {
        self.bytes.len()
    }
}
