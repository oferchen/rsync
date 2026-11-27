//! A directory's listing: the read the walk does when it enters a directory,
//! and the owned plan a streamed batch keeps for a directory it listed ahead
//! of the walk.

use std::borrow::Cow;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::super::super::create_failure::full_fname;
use super::super::planner::{DirectoryPlan, EntryAction, PlannedEntry, report_unstatable_entry};
use super::super::support::{DirectoryEntry, read_directory_entries_sorted_reuse};
use super::{ListedEntry, operand_root_fname};
use crate::local_copy::{CopyContext, DeleteTiming, LocalCopyError, SourceAttrs};

/// The entries a directory read produced, with the error the frame re-raises
/// once the directory is finalized.
pub(crate) struct DirectoryListing {
    pub(crate) entries: Vec<DirectoryEntry>,
    pub(crate) first_error: Option<LocalCopyError>,
    pub(crate) enumeration_failed: bool,
}

/// Reads `source` as the walk lists it, reporting the read failures upstream
/// reports while it builds the file list.
///
/// upstream: flist.c:send_directory()
pub(crate) fn read_directory(
    context: &mut CopyContext,
    source: &Path,
    relative: Option<&Path>,
    walks_children: bool,
    list_only: bool,
) -> DirectoryListing {
    let list_start = Instant::now();
    // upstream: flist.c:2276-2277 - the operand's directory is opened through
    // `open_sender_source_path()`, whose held root is re-checked against the
    // identity recorded at flist.c:302-305. A root whose identity changed -
    // e.g. `dir/symlink/..`, which flist.c:2869 `clean_fname()` collapses to a
    // different directory than the one stat'd - fails the scan with ELOOP
    // (flist.c:325-329), reported through the `opendir` arm below.
    let pinned_root_error = context.pinned_anchor_error(source);
    let scans_operand_root = relative.is_none() && context.source_anchor() == Some(source);
    context.set_batch_listing_dir(Some(relative.unwrap_or(Path::new("."))));
    let (readdir_buf, scan_root) = context.readdir_buf_with_confined_anchor();
    let mut first_error: Option<LocalCopyError> = None;
    // upstream: flist.c:2376-2387 - `send_directory()` emits this directory's
    // own entry and ACL (flist.c:2072-2083) BEFORE it opens the directory, so a
    // failed `opendir()` is an enumeration-only failure: upstream `return`s
    // without descending and the entry it already sent still reaches the
    // receiver, which mkdir's the destination directory and applies its
    // metadata and ACLs. The three arms are:
    //   ENOENT -> `interpret_stat_error()` (flist.c:2228-2238) warns
    //             `directory <name> has vanished` and sets IOERR_VANISHED (24);
    //   ENOTDIR with FLAG_PERHAPS_DIR -> silent return. No counterpart here:
    //             this frame is only entered for a source already stat'd as a
    //             directory, so the "perhaps" flag is never set and ENOTDIR
    //             falls through to the general arm exactly as upstream does
    //             when the flag is clear;
    //   otherwise -> IOERR_GENERAL (23) plus rsyserr `opendir %s failed`.
    // Abandoning the frame here instead would skip `ensure_directory` and
    // `apply_final_directory_metadata`, leaving an unreadable source
    // directory's own permissions/ACLs unapplied and a stale destination ACL
    // entry unrevoked. Continue with no children; the tail re-raises.
    let mut enumeration_failed = false;
    let mut unstatable = Vec::new();
    // upstream: flist.c:3003 - a directory this frame will not walk (no
    // recursion, not a trailing-slash or dot-dir source) is never opened, so an
    // unreadable one is not an error.
    let scan = if !walks_children {
        Ok(Vec::new())
    } else {
        match pinned_root_error {
            Some(error) => Err(LocalCopyError::io("open directory", source, error)),
            None => {
                read_directory_entries_sorted_reuse(source, readdir_buf, scan_root, &mut unstatable)
            }
        }
    };
    let mut entries = match scan {
        Ok(entries) => entries,
        Err(error) => {
            if error.is_vanished_error() {
                eprintln!("directory has vanished: {}", full_fname(source));
            } else {
                let detail = match error.kind() {
                    crate::local_copy::LocalCopyErrorKind::Io { source, .. } => {
                        crate::local_copy::upstream_io_error(source)
                    }
                    _ => error.to_string(),
                };
                // upstream: flist.c send_directory() - `opendir %s failed`
                // names the directory through full_fname(), so a relative
                // operand is reported prefixed by the working directory.
                let name = if scans_operand_root {
                    operand_root_fname(source)
                } else {
                    full_fname(source)
                };
                eprintln!("rsync: [sender] opendir {name} failed: {detail}");
                context.record_io_error();
            }
            first_error = Some(error);
            enumeration_failed = true;
            Vec::new()
        }
    };
    for (path, error) in unstatable {
        let name = path.file_name().map_or(path.as_path(), Path::new);
        let entry_relative = relative.map_or_else(|| name.to_path_buf(), |dir| dir.join(name));
        if let Some(vanished) = report_unstatable_entry(context, &path, &entry_relative, error)
            && first_error.is_none()
        {
            first_error = Some(vanished);
        }
    }
    // upstream: the file list is built once before the receiver mkdir's the
    // destination root, so a destination created inside the source tree (e.g.
    // `rsync -a src src/child`) never appears in the list. oc reads each source
    // directory live, after the destination root is created, so drop the entry
    // that IS the destination root to avoid descending into our own output.
    // A listing creates no destination, so there is nothing to drop.
    if !list_only {
        let destination_root = context.destination_root().to_path_buf();
        entries.retain(|entry| entry.path != destination_root);
    }
    context.record_file_list_generation(list_start.elapsed());
    context.reserve_event_capacity(entries.len());
    context.register_progress();
    DirectoryListing {
        entries,
        first_error,
        enumeration_failed,
    }
}

/// One planned entry that owns everything but its directory entry, which it
/// names by position in the listing.
struct DetachedEntry {
    index: usize,
    relative: PathBuf,
    action: EntryAction,
    metadata_override: Option<fs::Metadata>,
    attrs: SourceAttrs,
    listed: Option<ListedEntry>,
}

/// A [`DirectoryPlan`] kept apart from the entries it borrows, until the walk
/// enters the directory.
pub(crate) struct DetachedPlan {
    entries: Vec<DetachedEntry>,
    keep_names: Vec<OsString>,
    deletion_enabled: bool,
    delete_timing: Option<DeleteTiming>,
}

impl DetachedPlan {
    /// Detaches `plan`, whose entries all borrow from `entries`.
    pub(crate) fn detach(plan: DirectoryPlan<'_>, entries: &[DirectoryEntry]) -> Self {
        let base = entries.as_ptr().addr();
        let size = size_of::<DirectoryEntry>();
        let planned = plan
            .planned_entries
            .into_iter()
            .map(|planned| {
                let index = (std::ptr::from_ref(planned.entry).addr() - base) / size;
                debug_assert!(std::ptr::eq(&entries[index], planned.entry));
                DetachedEntry {
                    index,
                    relative: planned.relative,
                    action: planned.action,
                    metadata_override: planned.metadata_override,
                    attrs: planned.attrs,
                    listed: planned.listed,
                }
            })
            .collect();
        Self {
            entries: planned,
            keep_names: plan.keep_names.into_iter().map(Cow::into_owned).collect(),
            deletion_enabled: plan.deletion_enabled,
            delete_timing: plan.delete_timing,
        }
    }

    /// The plan again, borrowing the listing it was detached from.
    pub(crate) fn attach(self, entries: &[DirectoryEntry]) -> DirectoryPlan<'_> {
        DirectoryPlan {
            planned_entries: self
                .entries
                .into_iter()
                .map(|detached| PlannedEntry {
                    entry: &entries[detached.index],
                    relative: detached.relative,
                    action: detached.action,
                    metadata_override: detached.metadata_override,
                    attrs: detached.attrs,
                    listed: detached.listed,
                })
                .collect(),
            keep_names: self.keep_names.into_iter().map(Cow::Owned).collect(),
            deletion_enabled: self.deletion_enabled,
            delete_timing: self.delete_timing,
        }
    }

    /// The batch index and listing position of every subdirectory the walk
    /// descends into.
    pub(crate) fn subdirectories(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.entries.iter().filter_map(|detached| {
            let listed = detached.listed.as_ref()?;
            matches!(detached.action, EntryAction::CopyDirectory)
                .then(|| (listed.index() as usize, detached.index))
        })
    }
}

/// How listing a directory ended, at the step the walk would have stopped.
pub(crate) enum ListingOutcome {
    /// Entering the directory's merge files failed; the frame returns this
    /// before touching the destination.
    EnterFailed(LocalCopyError),
    /// Planning the entries failed; the frame returns this where it plans.
    PlanFailed(LocalCopyError),
    /// The directory's entries, planned, listed and in file-list order.
    Planned(DetachedPlan),
}

/// A directory listed ahead of the walk.
pub(crate) struct CachedListing {
    pub(crate) listing: DirectoryListing,
    pub(crate) outcome: ListingOutcome,
}
