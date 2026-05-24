//! Prototype: arena-allocated path storage for [`FileEntry`] (RSS-7).
//!
//! This module ships an opt-in, parallel `FileEntry` shape that swaps the
//! per-entry `PathBuf` heap allocation for either a bump-allocated path
//! borrowed from a [`bumpalo::Bump`] arena or a legacy owned `PathBuf`. It is
//! the prototype landing referenced by the RSS-5 design doc and the RSS-6
//! back-compat audit. The migration of every consumer (sort, filter, transfer)
//! is out of scope for RSS-7 and is tracked under RSS-9; here we keep the
//! production [`super::FileEntry`] surface untouched so default builds are
//! byte-identical.
//!
//! # Feature gate
//!
//! The prototype is gated behind `flist-arena-prototype`. With the feature
//! disabled the [`FilePath::Arena`] variant is uninhabited
//! (`std::convert::Infallible`), the arena dependency is not pulled in, and
//! [`ArenaFileEntry`] degrades to a thin wrapper around the legacy `PathBuf`.
//! With the feature enabled the same type can borrow `&'arena Path` directly
//! out of a [`bumpalo::Bump`] arena, removing the per-entry `PathBuf` heap
//! allocation along the build path.
//!
//! # Lifetime parameter
//!
//! [`ArenaFileEntry`] carries an `'arena` lifetime parameter regardless of
//! the feature flag. Default builds use `'static` (the uninhabited variant
//! never carries a real borrow), so external consumers that construct
//! `ArenaFileEntry<'static>` compile unchanged. Real arena borrows only
//! materialise when the feature is enabled and a `&'arena Bump` is supplied.

use std::path::{Path, PathBuf};

#[cfg(feature = "flist-arena-prototype")]
use bumpalo::Bump;

/// Uninhabited stand-in for the arena variant when the prototype is disabled.
///
/// Using [`std::convert::Infallible`] keeps [`FilePath`] a single shape on
/// every build while statically forbidding construction of the arena variant
/// in default builds. The `PhantomData<&'arena ()>` carries the lifetime so
/// the type still parameterises over `'arena` and the enum's variant layout
/// matches the feature-enabled shape.
#[cfg(not(feature = "flist-arena-prototype"))]
#[derive(Debug)]
pub struct ArenaBorrow<'arena> {
    never: std::convert::Infallible,
    _marker: std::marker::PhantomData<&'arena ()>,
}

#[cfg(not(feature = "flist-arena-prototype"))]
impl<'arena> Clone for ArenaBorrow<'arena> {
    fn clone(&self) -> Self {
        match self.never {}
    }
}

#[cfg(not(feature = "flist-arena-prototype"))]
impl<'arena> PartialEq for ArenaBorrow<'arena> {
    fn eq(&self, _other: &Self) -> bool {
        match self.never {}
    }
}

#[cfg(not(feature = "flist-arena-prototype"))]
impl<'arena> Eq for ArenaBorrow<'arena> {}

/// Path storage for [`ArenaFileEntry`].
///
/// Either an owned [`PathBuf`] (legacy shape, allocated per entry) or a path
/// borrowed from a bump arena (`&'arena Path`). The borrowed variant is only
/// constructible when the `flist-arena-prototype` feature is enabled; default
/// builds see `FilePath::Arena` as an uninhabited variant and the enum
/// behaves exactly like a single-variant `Owned(PathBuf)`.
#[derive(Debug)]
pub enum FilePath<'arena> {
    /// Owned path - legacy shape, one heap allocation per entry.
    Owned(PathBuf),
    /// Path borrowed from a [`bumpalo::Bump`] arena.
    ///
    /// Uninhabited when the `flist-arena-prototype` feature is disabled.
    #[cfg(feature = "flist-arena-prototype")]
    Arena(&'arena Path),
    /// Uninhabited placeholder preserving the `'arena` lifetime parameter
    /// when the prototype feature is disabled. Never constructible.
    #[cfg(not(feature = "flist-arena-prototype"))]
    Arena(ArenaBorrow<'arena>),
}

impl<'arena> FilePath<'arena> {
    /// Borrows the path uniformly regardless of storage variant.
    ///
    /// This is the single accessor downstream consumers should rely on
    /// during the prototype phase; it lets sort/filter/transfer code path
    /// continue to operate on `&Path` while RSS-9 stages the migration of
    /// each individual consumer.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Owned(p) => p.as_path(),
            #[cfg(feature = "flist-arena-prototype")]
            Self::Arena(p) => p,
            #[cfg(not(feature = "flist-arena-prototype"))]
            Self::Arena(never) => match never.never {},
        }
    }

    /// Returns true if the path is owned (legacy shape).
    #[must_use]
    pub fn is_owned(&self) -> bool {
        matches!(self, Self::Owned(_))
    }

    /// Returns true if the path is bump-arena-borrowed.
    ///
    /// Always false in default builds: the arena variant is uninhabited.
    #[must_use]
    pub fn is_arena(&self) -> bool {
        #[cfg(feature = "flist-arena-prototype")]
        {
            matches!(self, Self::Arena(_))
        }
        #[cfg(not(feature = "flist-arena-prototype"))]
        {
            false
        }
    }
}

impl<'arena> Clone for FilePath<'arena> {
    fn clone(&self) -> Self {
        match self {
            Self::Owned(p) => Self::Owned(p.clone()),
            #[cfg(feature = "flist-arena-prototype")]
            Self::Arena(p) => Self::Arena(p),
            #[cfg(not(feature = "flist-arena-prototype"))]
            Self::Arena(never) => match never.never {},
        }
    }
}

impl<'arena> PartialEq for FilePath<'arena> {
    fn eq(&self, other: &Self) -> bool {
        self.path() == other.path()
    }
}

impl<'arena> Eq for FilePath<'arena> {}

/// Prototype FileEntry shape with arena-allocatable path storage.
///
/// Mirrors the production [`super::FileEntry`] field set but routes the
/// `name` field through [`FilePath`] so the prototype can be benchmarked
/// against the legacy `PathBuf` allocation pattern without touching the
/// production type. The full migration of sort/filter/transfer consumers
/// is RSS-9; for now this type stands alone as the measurement vehicle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArenaFileEntry<'arena> {
    name: FilePath<'arena>,
    size: u64,
    mode: u32,
}

impl<'arena> ArenaFileEntry<'arena> {
    /// Returns the entry path as `&Path` regardless of storage variant.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.name.path()
    }

    /// Returns the underlying [`FilePath`] storage.
    #[must_use]
    pub fn file_path(&self) -> &FilePath<'arena> {
        &self.name
    }

    /// Returns the entry size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Returns the entry mode bits (file type + permissions).
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }
}

/// Builder for [`ArenaFileEntry`] that routes path allocation through an
/// optional bump arena.
///
/// Pass `Some(&Bump)` (under the feature) to bump-allocate the path; pass
/// `None` to fall back to the legacy owned `PathBuf` shape. The builder
/// preserves the existing public construction surface while letting the
/// prototype measure the arena path side-by-side.
pub struct ArenaFileEntryBuilder<'arena> {
    #[cfg(feature = "flist-arena-prototype")]
    arena: Option<&'arena Bump>,
    #[cfg(not(feature = "flist-arena-prototype"))]
    _marker: std::marker::PhantomData<&'arena ()>,
    size: u64,
    mode: u32,
}

impl<'arena> ArenaFileEntryBuilder<'arena> {
    /// Creates a builder that owns its path allocations (legacy shape).
    #[cfg(feature = "flist-arena-prototype")]
    #[must_use]
    pub fn owned() -> Self {
        Self {
            arena: None,
            size: 0,
            mode: 0,
        }
    }

    /// Creates a builder that owns its path allocations (legacy shape).
    #[cfg(not(feature = "flist-arena-prototype"))]
    #[must_use]
    pub fn owned() -> Self {
        Self {
            _marker: std::marker::PhantomData,
            size: 0,
            mode: 0,
        }
    }

    /// Creates a builder that bump-allocates paths into `arena`.
    ///
    /// Only available with the `flist-arena-prototype` feature enabled.
    #[cfg(feature = "flist-arena-prototype")]
    #[must_use]
    pub fn with_arena(arena: &'arena Bump) -> Self {
        Self {
            arena: Some(arena),
            size: 0,
            mode: 0,
        }
    }

    /// Sets the file size.
    #[must_use]
    pub const fn size(mut self, size: u64) -> Self {
        self.size = size;
        self
    }

    /// Sets the file mode (type + permissions).
    #[must_use]
    pub const fn mode(mut self, mode: u32) -> Self {
        self.mode = mode;
        self
    }

    /// Finalises the builder, allocating the path through the arena when one
    /// is configured and the prototype feature is enabled.
    #[must_use]
    pub fn build(self, path: &Path) -> ArenaFileEntry<'arena> {
        #[cfg(feature = "flist-arena-prototype")]
        let name = match self.arena {
            Some(bump) => FilePath::Arena(alloc_path_in_arena(bump, path)),
            None => FilePath::Owned(path.to_path_buf()),
        };
        #[cfg(not(feature = "flist-arena-prototype"))]
        let name = FilePath::Owned(path.to_path_buf());
        ArenaFileEntry {
            name,
            size: self.size,
            mode: self.mode,
        }
    }
}

#[cfg(feature = "flist-arena-prototype")]
fn alloc_path_in_arena<'arena>(arena: &'arena Bump, path: &Path) -> &'arena Path {
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        let slice: &'arena [u8] = arena.alloc_slice_copy(bytes);
        Path::new(OsStr::from_bytes(slice))
    }
    #[cfg(not(unix))]
    {
        // Non-Unix targets cannot round-trip arbitrary `OsStr` bytes; copy
        // the UTF-16-derived `String` representation into the arena instead.
        let s = path.to_string_lossy();
        let arena_str: &'arena str = arena.alloc_str(&s);
        Path::new(arena_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_path_round_trips_identically() {
        let original = PathBuf::from("src/lib/foo.rs");
        let entry = ArenaFileEntryBuilder::<'static>::owned()
            .size(1024)
            .mode(0o100644)
            .build(&original);

        assert!(entry.file_path().is_owned());
        assert!(!entry.file_path().is_arena());
        assert_eq!(entry.path(), Path::new("src/lib/foo.rs"));
        assert_eq!(entry.path(), original.as_path());
        assert_eq!(entry.size(), 1024);
        assert_eq!(entry.mode(), 0o100644);
    }

    #[cfg(feature = "flist-arena-prototype")]
    #[test]
    fn arena_path_round_trips_identically() {
        let arena = Bump::new();
        let original = PathBuf::from("src/lib/foo.rs");
        let entry = ArenaFileEntryBuilder::with_arena(&arena)
            .size(1024)
            .mode(0o100644)
            .build(&original);

        assert!(entry.file_path().is_arena());
        assert!(!entry.file_path().is_owned());
        assert_eq!(entry.path(), Path::new("src/lib/foo.rs"));
        assert_eq!(entry.path(), original.as_path());
        assert_eq!(entry.size(), 1024);
        assert_eq!(entry.mode(), 0o100644);
    }

    #[test]
    fn owned_and_arena_accessors_agree() {
        // Both variants must produce identical `path() -> &Path` results so
        // RSS-9 consumers can migrate field-by-field without behaviour change.
        let original = PathBuf::from("a/b/c.txt");
        let owned = ArenaFileEntryBuilder::<'static>::owned()
            .size(42)
            .mode(0o100600)
            .build(&original);

        assert_eq!(owned.path(), original.as_path());

        #[cfg(feature = "flist-arena-prototype")]
        {
            let arena = Bump::new();
            let arena_entry = ArenaFileEntryBuilder::with_arena(&arena)
                .size(42)
                .mode(0o100600)
                .build(&original);
            assert_eq!(arena_entry.path(), owned.path());
            assert_eq!(arena_entry.size(), owned.size());
            assert_eq!(arena_entry.mode(), owned.mode());
        }
    }

    #[cfg(unix)]
    #[cfg(feature = "flist-arena-prototype")]
    #[test]
    fn arena_path_preserves_non_utf8_bytes() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let arena = Bump::new();
        let raw = b"weird\xff\xfename";
        let original = PathBuf::from(OsStr::from_bytes(raw));
        let entry = ArenaFileEntryBuilder::with_arena(&arena)
            .size(0)
            .mode(0o100644)
            .build(&original);

        assert!(entry.file_path().is_arena());
        assert_eq!(entry.path().as_os_str().as_bytes(), raw);
    }
}
