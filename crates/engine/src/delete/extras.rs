//! Pure dest-vs-flist set subtraction for one directory.
//!
//! [`compute_extras`] is phase 1 of the parallel-deterministic-delete
//! pipeline: list every entry in `dest_dir`, drop any name that also
//! appears in the corresponding flist segment, classify what remains by
//! kind, and return the surviving [`DeleteEntry`] values to the caller.
//! The caller (segment-dispatch worker) wraps the result in a
//! [`super::DeletePlan`], sorts it via
//! [`super::DeletePlan::sort_by_name`], and publishes the plan into a
//! [`super::DeletePlanMap`] for the single emitter thread to drain.
//!
//! The function is intentionally side-effect-free apart from
//! `read_dir`/`symlink_metadata` syscalls on `dest_dir`; it never
//! unlinks, never opens files for reading, and never touches the
//! hardlink table. Hardlink-cohort tagging is available through the
//! `compute_extras_with_cohorts` variant, which reads a pre-built
//! `CohortIndex` snapshot instead of mutating hardlink state.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`
//!   (`delete_in_dir`): scans `get_dirlist(fbuf, ...)` then for every
//!   item calls `flist_find_ignore_dirness(cur_flist, fp) < 0` to decide
//!   whether to delete. We perform the same set subtraction in pure
//!   Rust: hash the segment's basenames, then walk the dest directory.
//! - `target/interop/upstream-src/rsync-3.4.1/flist.c:flist_find`
//!   (basename-keyed lookup against the active flist).

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use protocol::flist::FileEntry;

use super::cohort_index::CohortIndex;
use super::plan::{DeleteEntry, DeleteEntryKind};

/// Lists `dest_dir`, subtracts every basename that appears in
/// `segment_entries`, and classifies each surviving entry by kind.
///
/// The returned vector is **unsorted**. Callers wrap it in a
/// [`super::DeletePlan`] and call
/// [`super::DeletePlan::sort_by_name`] to lock in upstream's
/// `delete_in_dir` emission order before publishing the plan.
///
/// # Classification
///
/// Each surviving entry is classified via
/// [`fs::symlink_metadata`] (so symlinks are reported as
/// [`DeleteEntryKind::Symlink`] rather than the type of their target)
/// and mapped to a [`DeleteEntryKind`] bucket consistent with the
/// `DeleteStats` counters upstream tracks:
///
/// - regular file -> [`DeleteEntryKind::File`]
/// - directory -> [`DeleteEntryKind::Dir`]
/// - symlink -> [`DeleteEntryKind::Symlink`]
/// - block/char device -> [`DeleteEntryKind::Device`] (Unix only)
/// - FIFO/socket -> [`DeleteEntryKind::Special`] (Unix only)
/// - anything else -> [`DeleteEntryKind::File`] (matches upstream's
///   "anything not recognised counts as a regular deletion" fallback;
///   on Windows the platform never reports device/FIFO/socket types,
///   so non-dir, non-symlink entries land here).
///
/// # Errors
///
/// Returns the I/O error from [`fs::read_dir`] if `dest_dir` cannot be
/// opened (for example `NotFound` when the directory has been removed
/// between traversal planning and segment dispatch). Per-entry
/// `symlink_metadata` failures are propagated the same way: if the
/// destination's filesystem disappears mid-scan, the worker surfaces
/// the error instead of silently dropping an entry that may still need
/// deletion. The caller is expected to log + skip rather than abort.
pub fn compute_extras(
    dest_dir: &Path,
    segment_entries: &[FileEntry],
) -> io::Result<Vec<DeleteEntry>> {
    compute_extras_with_cohorts(dest_dir, segment_entries, None)
}

/// Variant of [`compute_extras`] that attaches a hardlink cohort tag to
/// each surviving entry whose destination basename matches a member of
/// the supplied [`CohortIndex`].
///
/// The cohort tag has no effect on the unlink decision itself; matching
/// upstream `delete.c:130-225`, every extras path is still unlinked
/// unconditionally and the kernel reconciles ref counts. The tag exists
/// so the [`super::DeleteEmitter`] can emit cohort-aware itemize lines
/// without re-statting and so future diagnostics can distinguish a
/// last-ref deletion from one of several in the same cohort.
///
/// `cohort_index = None` reproduces the original behaviour bit for bit
/// and is the path taken by callers that have not yet plumbed the
/// snapshot through.
///
/// # Errors
///
/// Same as [`compute_extras`]: any I/O failure on `read_dir` or
/// per-entry `symlink_metadata` is surfaced to the caller.
pub fn compute_extras_with_cohorts(
    dest_dir: &Path,
    segment_entries: &[FileEntry],
    cohort_index: Option<&Arc<CohortIndex>>,
) -> io::Result<Vec<DeleteEntry>> {
    let segment_names = segment_basenames(segment_entries);
    let read_dir = fs::read_dir(dest_dir)?;
    let mut extras = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let name = entry.file_name();
        if segment_names.contains(&name) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        let kind = classify(&metadata);
        let delete_entry = match cohort_index.and_then(|idx| idx.cohort_of(name.as_os_str())) {
            Some(cohort) => DeleteEntry::with_cohort(name, kind, cohort),
            None => DeleteEntry::new(name, kind),
        };
        extras.push(delete_entry);
    }
    Ok(extras)
}

/// Collects the leaf basenames from a slice of flist entries into a
/// hash set keyed by [`OsString`].
///
/// Entries whose path has no terminal component (the empty path) are
/// skipped: such rows cannot collide with any real directory entry and
/// upstream's `flist_find` would never match them either.
fn segment_basenames(entries: &[FileEntry]) -> HashSet<OsString> {
    let mut set = HashSet::with_capacity(entries.len());
    for entry in entries {
        if let Some(name) = entry.path().file_name() {
            set.insert(name.to_os_string());
        }
    }
    set
}

/// Maps a [`fs::Metadata`] to a [`DeleteEntryKind`].
///
/// Mirrors the platform-specific helpers in
/// `crates/engine/src/local_copy/executor/directory/support.rs`: on
/// Unix we consult `FileTypeExt` for device/FIFO/socket detection; on
/// non-Unix platforms `std::fs::FileType` exposes only file / dir /
/// symlink and everything else collapses to [`DeleteEntryKind::File`].
pub(super) fn classify(metadata: &fs::Metadata) -> DeleteEntryKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return DeleteEntryKind::Symlink;
    }
    if file_type.is_dir() {
        return DeleteEntryKind::Dir;
    }
    if file_type.is_file() {
        return DeleteEntryKind::File;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_block_device() || file_type.is_char_device() {
            return DeleteEntryKind::Device;
        }
        if file_type.is_fifo() || file_type.is_socket() {
            return DeleteEntryKind::Special;
        }
    }
    DeleteEntryKind::File
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::path::PathBuf;
    use tempfile::TempDir;

    use protocol::flist::FileEntry;

    fn flist_file(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 0, 0o644)
    }

    fn touch(dir: &Path, name: &str) {
        File::create(dir.join(name)).expect("create file");
    }

    #[test]
    fn empty_dest_and_empty_segment_yields_no_extras() {
        let dir = TempDir::new().unwrap();
        let extras = compute_extras(dir.path(), &[]).unwrap();
        assert!(extras.is_empty());
    }

    #[test]
    fn entries_present_in_segment_are_excluded() {
        let dir = TempDir::new().unwrap();
        for n in ["a", "b", "c"] {
            touch(dir.path(), n);
        }
        let segment = vec![flist_file("a"), flist_file("c")];
        let extras = compute_extras(dir.path(), &segment).unwrap();
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].name, OsString::from("b"));
        assert_eq!(extras[0].kind, DeleteEntryKind::File);
        assert!(extras[0].hardlink_cohort.is_none());
    }

    #[test]
    fn segment_with_full_paths_still_matches_on_basename() {
        // Sender-side flist rows often carry the full destination path
        // (e.g. "sub/a"), but upstream's `flist_find_ignore_dirness`
        // compares basenames. Verify our set-subtraction agrees.
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "a");
        touch(dir.path(), "b");
        let segment = vec![FileEntry::new_file(PathBuf::from("sub/a"), 0, 0o644)];
        let extras = compute_extras(dir.path(), &segment).unwrap();
        let names: Vec<&OsString> = extras.iter().map(|e| &e.name).collect();
        assert_eq!(names, vec![&OsString::from("b")]);
    }

    #[test]
    fn dest_only_entries_all_become_extras() {
        let dir = TempDir::new().unwrap();
        for n in ["x", "y"] {
            touch(dir.path(), n);
        }
        let mut extras = compute_extras(dir.path(), &[]).unwrap();
        extras.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&OsString> = extras.iter().map(|e| &e.name).collect();
        assert_eq!(names, vec![&OsString::from("x"), &OsString::from("y")]);
    }

    #[test]
    fn classifies_regular_file_directory_and_symlink() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "regular");
        fs::create_dir(dir.path().join("subdir")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("regular", dir.path().join("link")).unwrap();
        #[cfg(windows)]
        {
            // Symlinks on Windows require elevated privileges or
            // Developer Mode; fall back to a plain file so the
            // assertion below still has something to classify.
            touch(dir.path(), "link");
        }

        let mut extras = compute_extras(dir.path(), &[]).unwrap();
        extras.sort_by(|a, b| a.name.cmp(&b.name));
        let by_name: std::collections::HashMap<_, _> =
            extras.iter().map(|e| (e.name.clone(), e.kind)).collect();
        assert_eq!(by_name[&OsString::from("regular")], DeleteEntryKind::File);
        assert_eq!(by_name[&OsString::from("subdir")], DeleteEntryKind::Dir);
        #[cfg(unix)]
        assert_eq!(by_name[&OsString::from("link")], DeleteEntryKind::Symlink);
    }

    #[cfg(unix)]
    #[test]
    fn classifies_fifo_as_special() {
        use std::ffi::CString;
        let dir = TempDir::new().unwrap();
        let fifo = dir.path().join("pipe");
        let c_path = CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: mkfifo with a valid C string under a fresh TempDir
        // path is the standard libc fixture for FIFO tests; the call
        // does not touch Rust-owned memory.
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        if rc != 0 {
            // Some sandboxes refuse mkfifo (e.g. SIP-restricted dirs);
            // skip rather than fail.
            eprintln!("mkfifo unavailable in this environment, skipping");
            return;
        }
        let extras = compute_extras(dir.path(), &[]).unwrap();
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].name, OsString::from("pipe"));
        assert_eq!(extras[0].kind, DeleteEntryKind::Special);
    }

    #[test]
    fn nonexistent_dest_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        let err = compute_extras(&missing, &[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn results_are_deterministic_set_across_runs() {
        let dir = TempDir::new().unwrap();
        for n in ["m", "k", "z", "a"] {
            touch(dir.path(), n);
        }
        let segment = vec![flist_file("k")];
        let mut first = compute_extras(dir.path(), &segment).unwrap();
        let mut second = compute_extras(dir.path(), &segment).unwrap();
        first.sort_by(|a, b| a.name.cmp(&b.name));
        second.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(first, second);
        let names: Vec<&OsString> = first.iter().map(|e| &e.name).collect();
        assert_eq!(
            names,
            vec![
                &OsString::from("a"),
                &OsString::from("m"),
                &OsString::from("z"),
            ]
        );
    }

    #[test]
    fn ignores_segment_entries_with_empty_path() {
        // A degenerate row whose path has no leaf component must not
        // mask a real dest entry with the empty name (which itself
        // cannot exist on disk anyway).
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "real");
        let segment = vec![FileEntry::new_file(PathBuf::new(), 0, 0o644)];
        let extras = compute_extras(dir.path(), &segment).unwrap();
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].name, OsString::from("real"));
    }

    #[test]
    fn compute_extras_with_cohorts_none_matches_baseline() {
        // Passing `None` for the cohort index must reproduce the
        // behaviour of the original compute_extras byte for bit.
        let dir = TempDir::new().unwrap();
        for n in ["a", "b", "c"] {
            touch(dir.path(), n);
        }
        let segment = vec![flist_file("a")];
        let baseline = compute_extras(dir.path(), &segment).unwrap();
        let cohort_path = compute_extras_with_cohorts(dir.path(), &segment, None).unwrap();
        let mut baseline_sorted = baseline.clone();
        let mut cohort_sorted = cohort_path.clone();
        baseline_sorted.sort_by(|a, b| a.name.cmp(&b.name));
        cohort_sorted.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(baseline_sorted, cohort_sorted);
        for entry in &cohort_path {
            assert!(entry.hardlink_cohort.is_none());
        }
    }

    #[test]
    fn compute_extras_with_cohorts_tags_matching_basenames() {
        // The dest directory carries three extras; two of them share
        // basenames with a hardlink cohort in the segment. Those two
        // must come back tagged; the third must come back untagged.
        let dir = TempDir::new().unwrap();
        for n in ["alpha", "beta", "untagged"] {
            touch(dir.path(), n);
        }
        // Build a cohort segment with the two names. The segment is
        // separate from the segment we feed to compute_extras (which
        // excludes nothing here) - the cohort index is built once at
        // the receiver boundary, not derived from the same slice the
        // delete worker uses for set subtraction.
        let mut leader = FileEntry::new_file(PathBuf::from("alpha"), 0, 0o644);
        leader.set_hardlink_idx(u32::MAX);
        let mut member = FileEntry::new_file(PathBuf::from("beta"), 0, 0o644);
        member.set_hardlink_idx(0);
        let cohort_segment = vec![leader, member];
        let index = CohortIndex::build_from_flist_segment(&cohort_segment);
        let extras = compute_extras_with_cohorts(dir.path(), &[], Some(&index)).unwrap();
        let by_name: std::collections::HashMap<_, _> = extras
            .iter()
            .map(|e| (e.name.clone(), e.hardlink_cohort))
            .collect();
        let alpha_cohort = by_name[&OsString::from("alpha")];
        let beta_cohort = by_name[&OsString::from("beta")];
        let untagged_cohort = by_name[&OsString::from("untagged")];
        assert!(alpha_cohort.is_some());
        assert_eq!(alpha_cohort, beta_cohort, "same cohort id for both members");
        assert!(untagged_cohort.is_none());
    }

    #[test]
    fn compute_extras_with_cohorts_leaves_non_cohort_entries_untagged() {
        let dir = TempDir::new().unwrap();
        for n in ["x", "y"] {
            touch(dir.path(), n);
        }
        // Build a non-empty cohort index that has no overlap with the
        // destination names. Every extras row must come back without a
        // tag.
        let mut leader = FileEntry::new_file(PathBuf::from("unrelated"), 0, 0o644);
        leader.set_hardlink_idx(u32::MAX);
        let index = CohortIndex::build_from_flist_segment(&[leader]);
        let extras = compute_extras_with_cohorts(dir.path(), &[], Some(&index)).unwrap();
        assert_eq!(extras.len(), 2);
        for entry in extras {
            assert!(entry.hardlink_cohort.is_none());
        }
    }
}
