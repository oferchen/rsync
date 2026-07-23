//! `DeletePlan` and the entry types it holds.
//!
//! A [`DeletePlan`] is the per-directory work item produced by phase 1 of
//! the parallel-deterministic-delete pipeline (`compute_extras`) and
//! consumed by phase 2 (the single emitter). Plans are publish-once: once
//! a worker hands one to the [`super::DeletePlanMap`] it is frozen.
//!
//! # Ordering
//!
//! Inside a plan, entries are ordered to match upstream
//! `delete_in_dir`'s loop (`generator.c:320`), which iterates the sorted
//! destination directory list in reverse. Callers obtain that ordering by
//! sorting with [`super::super::delete::DeletePlan::sort_by_name`], which
//! uses upstream's protocol-29 `f_name_cmp` (files before directories)
//! ascending and then reverses the slice in place.
//!
//! # Hardlink Cohort
//!
//! Each [`DeleteEntry`] optionally carries a [`HardlinkCohortId`]. The
//! delete sweep itself does not consult the hardlink table to choose what
//! to remove (see section 6 of
//! `docs/design/parallel-deterministic-delete.md`), but the cohort id is
//! tracked here so downstream diagnostics and the emitter can tag
//! itemize lines, attribute deletions to a leader, and avoid double-stat
//! work when the leader has already been seen. `None` means the entry is
//! not part of any tracked hardlink group.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::PathBuf;

use protocol::flist::{FileEntry, FileType, compare_file_entries};

/// Identifier shared by all destination-side entries that belong to the
/// same hardlink group (same `(dev, ino)` pair).
///
/// Wraps the leader's file-list index, matching upstream's
/// `first_ndx` field on `struct hlink` (upstream: `hlink.c`). Using a
/// distinct newtype keeps callers from accidentally mixing a cohort id
/// with an unrelated file index.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct HardlinkCohortId(pub u32);

impl HardlinkCohortId {
    /// Wraps a leader file-list index as a hardlink cohort id.
    #[must_use]
    pub const fn new(leader_ndx: u32) -> Self {
        Self(leader_ndx)
    }

    /// Returns the wrapped leader index.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Type category of a destination-side entry slated for deletion.
///
/// Mirrors the buckets tracked by [`protocol::stats::DeleteStats`]: each
/// successful deletion increments exactly one counter. Devices collapse
/// block and character devices into a single bucket the same way
/// `DeleteStats::devices` does.
///
/// # Upstream Reference
///
/// - `stats.deleted_files` / `stats.deleted_dirs` /
///   `stats.deleted_symlinks` / `stats.deleted_devices` /
///   `stats.deleted_specials` in upstream `main.c` and `generator.c`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum DeleteEntryKind {
    /// Regular file (`S_IFREG`).
    File,
    /// Directory (`S_IFDIR`).
    Dir,
    /// Symbolic link (`S_IFLNK`).
    Symlink,
    /// Block or character device (`S_IFBLK` / `S_IFCHR`).
    Device,
    /// FIFO or Unix domain socket (`S_IFIFO` / `S_IFSOCK`).
    Special,
}

impl DeleteEntryKind {
    /// Classifies a [`FileType`] into the matching delete-stats bucket.
    ///
    /// Unknown or unmapped modes fall through to [`Self::File`] so the
    /// emitter still produces a deterministic itemize line.
    #[must_use]
    pub const fn from_file_type(ft: FileType) -> Self {
        match ft {
            FileType::Regular => Self::File,
            FileType::Directory => Self::Dir,
            FileType::Symlink => Self::Symlink,
            FileType::BlockDevice | FileType::CharDevice => Self::Device,
            FileType::Fifo | FileType::Socket => Self::Special,
        }
    }
}

/// A single destination-side entry slated for deletion.
///
/// The [`DeletePlan`] keeps the entries in upstream `delete_in_dir`
/// emission order. Each entry carries the leaf [`OsString`] name (relative
/// to the plan's directory), the kind for stats bookkeeping, and an
/// optional hardlink-cohort tag.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DeleteEntry {
    /// Leaf filename inside the plan's directory.
    pub name: OsString,
    /// Category bucket used by `DeleteStats` and itemize formatting.
    pub kind: DeleteEntryKind,
    /// Hardlink cohort the entry belongs to, if any. `None` means the
    /// destination entry is not part of a tracked hardlink group.
    pub hardlink_cohort: Option<HardlinkCohortId>,
}

impl DeleteEntry {
    /// Constructs a plain entry with no hardlink cohort attached.
    #[must_use]
    pub fn new(name: OsString, kind: DeleteEntryKind) -> Self {
        Self {
            name,
            kind,
            hardlink_cohort: None,
        }
    }

    /// Constructs an entry tagged with a hardlink cohort id.
    #[must_use]
    pub fn with_cohort(name: OsString, kind: DeleteEntryKind, cohort: HardlinkCohortId) -> Self {
        Self {
            name,
            kind,
            hardlink_cohort: Some(cohort),
        }
    }
}

/// Sorted, frozen list of destination entries to delete in one directory.
///
/// Construct an unsorted plan with [`Self::new`], append entries via
/// [`Self::push`], then call [`Self::sort_by_name`] to lock in
/// upstream's per-directory ordering. The plan tracks whether
/// [`Self::sort_by_name`] has been called so callers can assert the
/// ordering invariant before publication.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DeletePlan {
    /// Destination-relative directory the plan applies to.
    pub directory: PathBuf,
    /// Entries to delete. The slice is in upstream emission order
    /// (`f_name_cmp` ascending, reversed) once [`Self::sort_by_name`]
    /// has been called.
    pub extras: Vec<DeleteEntry>,
    sorted: bool,
}

impl DeletePlan {
    /// Creates an empty plan for the given destination-relative directory.
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            extras: Vec::new(),
            sorted: false,
        }
    }

    /// Creates a plan pre-populated with the given extras.
    ///
    /// The plan is marked unsorted; callers must invoke
    /// [`Self::sort_by_name`] before publication.
    #[must_use]
    pub fn from_extras(directory: PathBuf, extras: Vec<DeleteEntry>) -> Self {
        Self {
            directory,
            extras,
            sorted: false,
        }
    }

    /// Appends one entry to the plan and marks it unsorted.
    pub fn push(&mut self, entry: DeleteEntry) {
        self.extras.push(entry);
        self.sorted = false;
    }

    /// Returns the number of entries in the plan.
    #[must_use]
    pub fn len(&self) -> usize {
        self.extras.len()
    }

    /// Returns `true` when the plan has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.extras.is_empty()
    }

    /// Reports whether [`Self::sort_by_name`] has been called since the
    /// last mutation.
    #[must_use]
    pub fn is_sorted(&self) -> bool {
        self.sorted
    }

    /// Sorts the entries into upstream `delete_in_dir` emission order.
    ///
    /// Order is upstream's protocol-29 `f_name_cmp` ascending applied to a
    /// temporary [`FileEntry`] per entry (using the plan's `directory` as
    /// the parent), then reversed in place. The comparator is
    /// [`compare_file_entries`], which honours the `t_PATH`/`t_ITEM`
    /// distinction (upstream: `flist.c:3223`): within a directory,
    /// non-directories sort before directories, so `get_dirlist`'s sorted
    /// order places files first and directories last. A plain byte sort
    /// would instead order dirs and files by name alone, so an
    /// upper-case directory name like `D` would wrongly sort ahead of a
    /// file `a` - diverging from upstream's traversal and, under
    /// `--max-delete`, changing which entries survive when the cap trips.
    /// The sort is unstable to match upstream's `qsort` choice (upstream:
    /// `flist.c:3252-3378`, `generator.c:320`).
    pub fn sort_by_name(&mut self) {
        let dir = &self.directory;
        self.extras.sort_unstable_by(|a, b| {
            compare_file_entries(&entry_as_file_entry(dir, a), &entry_as_file_entry(dir, b))
        });
        // Upstream's `delete_in_dir` iterates the sorted dirlist in
        // reverse (`for (i = dirlist->used; i--; )`), so the emission
        // order is `f_name_cmp` ascending reversed.
        self.extras.reverse();
        self.sorted = true;
    }

    /// Convenience: returns the comparator that orders two [`DeleteEntry`]
    /// values in upstream ascending order under the plan's directory.
    /// Useful for callers that want to merge externally sorted candidate
    /// lists without rebuilding [`FileEntry`] values. Uses the same
    /// dir-aware [`compare_file_entries`] key as [`Self::sort_by_name`].
    #[must_use]
    pub fn ascending_order(&self, a: &DeleteEntry, b: &DeleteEntry) -> Ordering {
        let dir = &self.directory;
        compare_file_entries(&entry_as_file_entry(dir, a), &entry_as_file_entry(dir, b))
    }
}

/// Builds a transient [`FileEntry`] for one [`DeleteEntry`] so that
/// [`compare_file_entries`] can score it against another entry in the
/// same directory. The entry's mode is set from its `kind` so the
/// protocol-29 comparator applies its directory-vs-file disambiguation
/// (`is_dir()` is honoured for the `t_PATH`/`t_ITEM` ordering).
fn entry_as_file_entry(dir: &std::path::Path, e: &DeleteEntry) -> FileEntry {
    let full = dir.join(&e.name);
    match e.kind {
        DeleteEntryKind::Dir => FileEntry::new_directory(full, 0o755),
        DeleteEntryKind::Symlink => FileEntry::new_symlink(full, std::path::PathBuf::from("")),
        DeleteEntryKind::File | DeleteEntryKind::Device | DeleteEntryKind::Special => {
            FileEntry::new_file(full, 0, 0o644)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file_entry(name: &str) -> DeleteEntry {
        DeleteEntry::new(OsString::from(name), DeleteEntryKind::File)
    }

    #[test]
    fn cohort_id_roundtrips() {
        let id = HardlinkCohortId::new(42);
        assert_eq!(id.get(), 42);
        assert_eq!(id, HardlinkCohortId(42));
    }

    #[test]
    fn entry_kind_from_file_type_maps_all_variants() {
        assert_eq!(
            DeleteEntryKind::from_file_type(FileType::Regular),
            DeleteEntryKind::File
        );
        assert_eq!(
            DeleteEntryKind::from_file_type(FileType::Directory),
            DeleteEntryKind::Dir
        );
        assert_eq!(
            DeleteEntryKind::from_file_type(FileType::Symlink),
            DeleteEntryKind::Symlink
        );
        assert_eq!(
            DeleteEntryKind::from_file_type(FileType::BlockDevice),
            DeleteEntryKind::Device
        );
        assert_eq!(
            DeleteEntryKind::from_file_type(FileType::CharDevice),
            DeleteEntryKind::Device
        );
        assert_eq!(
            DeleteEntryKind::from_file_type(FileType::Fifo),
            DeleteEntryKind::Special
        );
        assert_eq!(
            DeleteEntryKind::from_file_type(FileType::Socket),
            DeleteEntryKind::Special
        );
    }

    #[test]
    fn entry_constructors_set_cohort_correctly() {
        let plain = DeleteEntry::new(OsString::from("x"), DeleteEntryKind::File);
        assert!(plain.hardlink_cohort.is_none());
        let tagged = DeleteEntry::with_cohort(
            OsString::from("y"),
            DeleteEntryKind::File,
            HardlinkCohortId::new(7),
        );
        assert_eq!(tagged.hardlink_cohort, Some(HardlinkCohortId::new(7)));
    }

    #[test]
    fn new_plan_is_empty_and_unsorted() {
        let plan = DeletePlan::new(PathBuf::from("sub"));
        assert!(plan.is_empty());
        assert_eq!(plan.len(), 0);
        assert!(!plan.is_sorted());
    }

    #[test]
    fn push_marks_plan_unsorted() {
        let mut plan = DeletePlan::new(PathBuf::from("sub"));
        plan.push(file_entry("a"));
        plan.sort_by_name();
        assert!(plan.is_sorted());
        plan.push(file_entry("b"));
        assert!(!plan.is_sorted());
    }

    #[test]
    fn sort_by_name_matches_upstream_reverse_order() {
        // Upstream `delete_in_dir` walks the sorted dirlist in reverse,
        // so for plain ASCII names the emission order is descending.
        let mut plan = DeletePlan::new(PathBuf::from("sub"));
        for n in ["c", "a", "d", "b"] {
            plan.push(file_entry(n));
        }
        plan.sort_by_name();
        let names: Vec<&str> = plan
            .extras
            .iter()
            .map(|e| e.name.to_str().unwrap())
            .collect();
        // Ascending `f_name_cmp` would be [a, b, c, d]; reversed -> [d, c, b, a].
        assert_eq!(names, vec!["d", "c", "b", "a"]);
        assert!(plan.is_sorted());
    }

    #[test]
    fn sort_preserves_cohort_tags() {
        let cohort = HardlinkCohortId::new(5);
        let mut plan = DeletePlan::new(PathBuf::from("sub"));
        plan.push(DeleteEntry::with_cohort(
            OsString::from("z"),
            DeleteEntryKind::File,
            cohort,
        ));
        plan.push(DeleteEntry::new(OsString::from("a"), DeleteEntryKind::File));
        plan.sort_by_name();
        // Reversed ascending -> z first, then a.
        assert_eq!(plan.extras[0].name, OsString::from("z"));
        assert_eq!(plan.extras[0].hardlink_cohort, Some(cohort));
        assert_eq!(plan.extras[1].name, OsString::from("a"));
        assert_eq!(plan.extras[1].hardlink_cohort, None);
    }

    #[test]
    fn sort_orders_files_before_dirs_like_upstream() {
        // Upstream's protocol-29 f_name_cmp sorts non-directories before
        // directories within a directory (t_ITEM before t_PATH), then by
        // name. Ascending: [a(symlink), c(special), b(dir)]; the reverse
        // that `sort_by_name` applies yields [b, c, a]. A plain byte sort
        // would instead give [c, b, a] because it ignores the kind - the
        // bug this ordering fix corrects.
        let mut plan = DeletePlan::new(PathBuf::from("d"));
        plan.push(DeleteEntry::new(OsString::from("b"), DeleteEntryKind::Dir));
        plan.push(DeleteEntry::new(
            OsString::from("a"),
            DeleteEntryKind::Symlink,
        ));
        plan.push(DeleteEntry::new(
            OsString::from("c"),
            DeleteEntryKind::Special,
        ));
        plan.sort_by_name();
        let names: Vec<&str> = plan
            .extras
            .iter()
            .map(|e| e.name.to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["b", "c", "a"]);
    }

    #[test]
    fn sort_emits_dirs_before_files_when_dir_name_sorts_first() {
        // Regression for the --max-delete survivor divergence: an
        // upper-case directory name `D` byte-sorts ahead of files `a1`,
        // `a2`, so a plain byte sort would emit [D, a2, a1] reversed ->
        // [a2, a1, D], deleting the files first. Upstream's dir-after-file
        // ordering makes the ascending order [a1, a2, D], reversed to
        // [D, a2, a1], so the directory is processed first and the cap
        // trips inside it - matching upstream's traversal.
        let mut plan = DeletePlan::new(PathBuf::from("dest"));
        plan.push(DeleteEntry::new(OsString::from("D"), DeleteEntryKind::Dir));
        plan.push(DeleteEntry::new(
            OsString::from("a1"),
            DeleteEntryKind::File,
        ));
        plan.push(DeleteEntry::new(
            OsString::from("a2"),
            DeleteEntryKind::File,
        ));
        plan.sort_by_name();
        let names: Vec<&str> = plan
            .extras
            .iter()
            .map(|e| e.name.to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["D", "a2", "a1"]);
    }

    #[test]
    fn ascending_order_matches_f_name_cmp() {
        let plan = DeletePlan::new(PathBuf::from("d"));
        let a = file_entry("aaa");
        let b = file_entry("bbb");
        assert_eq!(plan.ascending_order(&a, &b), Ordering::Less);
        assert_eq!(plan.ascending_order(&b, &a), Ordering::Greater);
        assert_eq!(plan.ascending_order(&a, &a), Ordering::Equal);
    }

    #[test]
    fn from_extras_takes_unsorted_input() {
        let entries = vec![file_entry("z"), file_entry("a")];
        let plan = DeletePlan::from_extras(PathBuf::from("d"), entries);
        assert_eq!(plan.len(), 2);
        assert!(!plan.is_sorted());
    }
}
