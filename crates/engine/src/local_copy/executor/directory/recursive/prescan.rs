//! Lists directories ahead of a streamed batch's walk.
//!
//! rsync's sender keeps at least `MIN_FILECNT_LOOKAHEAD` entries listed past
//! the file it is sending, building each sub-list before the data that needs
//! it. A streamed batch places sub-lists between records the same way, so a
//! record waits until the lists that precede it are known. The walk lists a
//! directory only when it enters it; [`BatchPrescan`] lists the directories
//! after it in send order as soon as a record would wait, and keeps each
//! listing until the walk enters that directory.
//!
//! Every listing is the one the walk produces on entry: the same read, the
//! same merge-file loading on a filter stack that follows the listed
//! directory, the same plan and file-list capture. The walk then takes the
//! listing instead of reading the directory itself.
//!
//! # Upstream Reference
//!
//! - `sender.c:523-555` - `send_extra_file_list(f, MIN_FILECNT_LOOKAHEAD)`
//!   before each request
//! - `flist.c:send_extra_file_list()` - sub-lists built depth-first, each
//!   directory pushing its local filters (`exclude.c:push_local_filters()`)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::batch::{list_batch_directory, order_for_batch_stream};
use super::listing::{CachedListing, DetachedPlan, ListingOutcome, read_directory};
use super::super::planner::plan_directory_entries;
use crate::local_copy::context::{DirectoryFilterGuard, DirectoryFilterHandles};
use crate::local_copy::{CopyContext, LocalCopyError};

/// How far ahead the walk lists, for tests that pin the batch to not
/// depend on it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrescanPolicy {
    /// Only what the walk enters.
    Never,
    /// As far as the lookahead needs, as rsync's sender does.
    Lookahead,
    /// Every directory, as early as possible.
    Everything,
}

#[cfg(test)]
thread_local! {
    static POLICY: std::cell::Cell<PrescanPolicy> =
        const { std::cell::Cell::new(PrescanPolicy::Lookahead) };
    static PEAK_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Sets how far this thread's next streamed batches list ahead.
#[cfg(test)]
pub(crate) fn set_prescan_policy(policy: PrescanPolicy) {
    POLICY.with(|cell| cell.set(policy));
}

/// The most listing entries the last streamed batch on this thread held.
#[cfg(test)]
pub(crate) fn last_peak_listed_entries() -> usize {
    PEAK_ENTRIES.with(std::cell::Cell::get)
}

/// Directory listings made ahead of the walk, and the filter stack that
/// lists them.
pub(crate) struct BatchPrescan {
    /// The source-side merge-file stack as the lister walks it, apart from
    /// the walk's own.
    filters: DirectoryFilterHandles,
    /// The directories whose merge files are on `filters`, outermost first.
    frames: Vec<(Option<PathBuf>, DirectoryFilterGuard)>,
    /// Listings the walk has not entered yet.
    listings: HashMap<Option<PathBuf>, CachedListing>,
    /// Source path of each subdirectory the walk will enter, by batch index.
    sources: HashMap<usize, PathBuf>,
    root_device: Option<u64>,
    held_entries: usize,
    peak_entries: usize,
}

impl BatchPrescan {
    fn new(filters: DirectoryFilterHandles, root_device: Option<u64>) -> Self {
        Self {
            filters,
            frames: Vec::new(),
            listings: HashMap::new(),
            sources: HashMap::new(),
            root_device,
            held_entries: 0,
            peak_entries: 0,
        }
    }
}

impl Drop for BatchPrescan {
    fn drop(&mut self) {
        // Guards pop their own frames: innermost first.
        while self.frames.pop().is_some() {}
        #[cfg(test)]
        PEAK_ENTRIES.with(|cell| cell.set(self.peak_entries));
    }
}

/// Whether directory `outer` encloses `inner`, `None` being the transfer
/// root.
fn encloses(outer: Option<&Path>, inner: Option<&Path>) -> bool {
    match (outer, inner) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(outer), Some(inner)) => inner != outer && inner.starts_with(outer),
    }
}

/// The listing of the directory the walk is entering, made now or earlier.
pub(crate) fn take_listing(
    context: &mut CopyContext,
    source: &Path,
    relative: Option<&Path>,
    root_device: Option<u64>,
    walks_children: bool,
) -> Result<CachedListing, LocalCopyError> {
    let mut prescan = match context.take_batch_prescan() {
        Some(prescan) => prescan,
        None => BatchPrescan::new(context.detached_dir_merge(), root_device),
    };
    let result = take_from(context, &mut prescan, source, relative, walks_children);
    context.restore_batch_prescan(prescan);
    result
}

fn take_from(
    context: &mut CopyContext,
    prescan: &mut BatchPrescan,
    source: &Path,
    relative: Option<&Path>,
    walks_children: bool,
) -> Result<CachedListing, LocalCopyError> {
    let key = relative.map(Path::to_path_buf);
    loop {
        if let Some(found) = prescan.listings.remove(&key) {
            prescan.held_entries -= found.listing.entries.len();
            return Ok(found);
        }
        // Lists before this one in send order are listed first, so each list
        // is built against the filter stack of its own ancestors.
        match next_in_send_order(context, prescan) {
            Some((dir, Some(dir_source))) if key.as_deref() != Some(dir.as_path()) => {
                list_into(context, prescan, &dir_source, Some(&dir), true)?;
            }
            Some((dir, None)) if key.as_deref() != Some(dir.as_path()) => {
                context.release_unlisted_batch_dir(&dir)?;
            }
            _ => list_into(context, prescan, source, relative, walks_children)?,
        }
    }
}

/// Lists ahead before the walk handles entry `id`, while its record would
/// wait for a list that is not known yet.
///
/// upstream: sender.c:523-555
pub(crate) fn list_ahead(context: &mut CopyContext, id: usize) -> Result<(), LocalCopyError> {
    let Some(mut prescan) = context.take_batch_prescan() else {
        return Ok(());
    };
    let result = list_ahead_with(context, &mut prescan, id);
    context.restore_batch_prescan(prescan);
    result
}

fn list_ahead_with(
    context: &mut CopyContext,
    prescan: &mut BatchPrescan,
    id: usize,
) -> Result<(), LocalCopyError> {
    while wants_lists(context, id) {
        let Some((dir, dir_source)) = next_in_send_order(context, prescan) else {
            break;
        };
        match dir_source {
            Some(dir_source) => list_into(context, prescan, &dir_source, Some(&dir), true)?,
            None => context.release_unlisted_batch_dir(&dir)?,
        }
    }
    Ok(())
}

#[cfg(not(test))]
fn wants_lists(context: &CopyContext, id: usize) -> bool {
    context.batch_lists_wanted(id)
}

#[cfg(test)]
fn wants_lists(context: &CopyContext, id: usize) -> bool {
    match POLICY.with(std::cell::Cell::get) {
        PrescanPolicy::Never => false,
        PrescanPolicy::Lookahead => context.batch_lists_wanted(id),
        PrescanPolicy::Everything => true,
    }
}

/// The next directory in send order whose list is not known, with its source
/// path when the walk would enter it.
fn next_in_send_order(
    context: &CopyContext,
    prescan: &mut BatchPrescan,
) -> Option<(PathBuf, Option<PathBuf>)> {
    let (id, dir) = context.next_unscanned_batch_dir()?;
    Some((dir, prescan.sources.remove(&id)))
}

/// Lists one directory as the walk would on entering it, and keeps the
/// listing for the walk.
fn list_into(
    context: &mut CopyContext,
    prescan: &mut BatchPrescan,
    source: &Path,
    relative: Option<&Path>,
    walks_children: bool,
) -> Result<(), LocalCopyError> {
    while prescan
        .frames
        .last()
        .is_some_and(|(outer, _)| !encloses(outer.as_deref(), relative))
    {
        prescan.frames.pop();
    }
    let list_only = context.list_only_enabled();
    let listing = read_directory(context, source, relative, walks_children, list_only);
    let root_device = prescan.root_device;
    let mut filters = prescan.filters.clone();
    let (guard, outcome) = context.with_dir_merge(&mut filters, |context| {
        let guard = match context.enter_directory(source, relative) {
            Ok(guard) => guard,
            Err(error) => return (None, ListingOutcome::EnterFailed(error)),
        };
        let outcome = match plan_directory_entries(context, &listing.entries, relative, root_device)
        {
            Err(error) => ListingOutcome::PlanFailed(error),
            Ok(mut plan) => {
                context.set_batch_listing_dir(None);
                match list_batch_directory(context, relative, &mut plan.planned_entries) {
                    Err(error) => ListingOutcome::PlanFailed(error),
                    Ok(()) => {
                        order_for_batch_stream(&mut plan.planned_entries);
                        ListingOutcome::Planned(DetachedPlan::detach(plan, &listing.entries))
                    }
                }
            }
        };
        (Some(guard), outcome)
    });
    context.set_batch_listing_dir(None);
    if let Some(guard) = guard {
        prescan.frames.push((relative.map(Path::to_path_buf), guard));
    }
    match &outcome {
        ListingOutcome::Planned(plan) => {
            for (index, position) in plan.subdirectories() {
                prescan
                    .sources
                    .insert(index, listing.entries[position].path.clone());
            }
        }
        // A directory the walk stops at keeps an empty sub-list, as when it
        // was never read.
        ListingOutcome::EnterFailed(_) | ListingOutcome::PlanFailed(_) => {
            context.release_unlisted_batch_dir(relative.unwrap_or(Path::new("")))?;
        }
    }
    prescan.held_entries += listing.entries.len();
    prescan.peak_entries = prescan.peak_entries.max(prescan.held_entries);
    prescan
        .listings
        .insert(relative.map(Path::to_path_buf), CachedListing { listing, outcome });
    Ok(())
}
