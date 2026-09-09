//! Top-level source processing orchestration and deferred operation flushing.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use logging::info_log;
use protocol::flist::{FileEntry, compare_file_entries};

use crate::local_copy::overrides::device_identifier;
use crate::local_copy::{
    CopyContext, CopyOutcome, DeleteTiming, LocalCopyAction, LocalCopyArgumentError,
    LocalCopyChangeSet, LocalCopyError, LocalCopyExecution, LocalCopyMetadata, LocalCopyOptions,
    LocalCopyPlan, LocalCopyRecord, LocalCopyRecordHandler, SourceSpec, delete_extraneous_entries,
};

use super::super::file::remove_existing_destination;
use super::super::non_empty_path;
use super::super::transcode_filename_component;
use super::destination::{ensure_destination_directory, query_destination_state};
use super::handlers::{
    handle_directory_contents_copy, handle_directory_copy, handle_non_directory_source,
};
use super::metadata::{compute_relative_paths, fetch_source_metadata, operand_stat_path};
use super::types::{SourceMetadataResult, SourceProcessingContext};

/// Returns the current time truncated to whole seconds since the Unix epoch.
///
/// Mirrors upstream's `time(NULL)` (main.c:327,1763), whose `time_t` result has
/// whole-second resolution. The transfer rate uses the integer difference of
/// two such marks (main.c:422), so the wall-clock span must be captured the same
/// way. A clock that reads before the epoch (never expected in practice) yields
/// `0` rather than panicking.
fn whole_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_secs())
        .unwrap_or(0)
}

/// Returns the operands in the order the local-copy executor should process
/// them so multi-operand itemize / `--list-only` output matches upstream.
///
/// upstream: flist.c:2544 flist_sort_and_clean() sorts the COMBINED
/// multi-source file list with f_name_cmp before the generator itemizes it, so
/// top-level operands are emitted in name order, not command-line order. oc's
/// per-directory walk already emits each operand's subtree in f_name_cmp order,
/// and a directory sorts contiguously with its own contents (no top-level
/// sibling can slot between a directory and its children), so reproducing
/// upstream's global order only requires ordering the operands themselves with
/// the same comparator the network generator applies to its flist
/// (`protocol::flist::compare_file_entries`, transfer `generator/file_list`).
///
/// Scope: applied only when every operand contributes a single NAMED top-level
/// entry - a non-`--relative` transfer with no trailing-slash / copy-contents
/// operand. A copy-contents operand splays its CONTENTS across the destination
/// root, which upstream interleaves across operands by content name; that is
/// handled by [`merged_contents_worklist`] instead, so those transfers never
/// reach this comparator. `--relative` operands carry implied parent
/// directories whose ordering is handled during emission, so they are left
/// untouched. Reordering is a pure output-ordering change: the destination
/// result is byte-identical, the `--delete` keep-set is order-independent, and
/// dir creation simply follows the same sorted order upstream uses.
fn ordered_operands(sources: &[SourceSpec], relative_paths: bool) -> Vec<&SourceSpec> {
    let mut ordered: Vec<&SourceSpec> = sources.iter().collect();
    let reorderable = sources.len() > 1
        && !relative_paths
        && sources.iter().all(|source| !source.copy_contents());
    if reorderable {
        ordered.sort_by(|a, b| compare_operand_names(a, b));
    }
    ordered
}

/// Compares two named operands with the flist sort comparator upstream applies
/// to top-level entries: directories compare as `name/` and a file sorts before
/// a same-prefixed directory, exactly as `compare_file_entries` orders the
/// generator's flist.
fn compare_operand_names(a: &SourceSpec, b: &SourceSpec) -> Ordering {
    compare_file_entries(&operand_sort_entry(a), &operand_sort_entry(b))
}

/// Models an operand as a top-level flist entry (its destination basename plus
/// its on-disk directory-ness) purely for ordering.
///
/// link_stat (lstat) semantics: a directory operand is a directory; a symlink
/// operand is not (upstream keeps the symlink's own name in the flist). An
/// operand that has vanished or cannot be stat'd is ordered as a file;
/// `process_single_source` reports its `link_stat` failure downstream.
fn operand_sort_entry(source: &SourceSpec) -> FileEntry {
    let name = source
        .path()
        .file_name()
        .map_or_else(|| source.path().to_path_buf(), PathBuf::from);
    if fs::symlink_metadata(source.path()).is_ok_and(|meta| meta.is_dir()) {
        FileEntry::new_directory(name, 0o755)
    } else {
        FileEntry::new_file(name, 0, 0o644)
    }
}

/// A destination-root entry synthesized from the union of every copy-contents
/// source's immediate children, deduplicated by name.
struct MergedRootEntry {
    /// `true` when the winning entry is a directory (lstat semantics).
    is_dir: bool,
    /// The child's basename, the destination path component and sort key.
    name: OsString,
    /// The contributing source paths in command-line order. A file keeps only
    /// the first (upstream's dedup keeps the first operand's copy); a directory
    /// keeps every contributor so their contents merge under one dest dir.
    paths: Vec<PathBuf>,
}

/// Builds the merged, globally-sorted work list for a multi-source COPY-CONTENTS
/// transfer, or `None` when the transfer is not that shape (the caller then
/// falls back to [`ordered_operands`]).
///
/// upstream: flist.c:2227 send_file_list() accumulates every source operand into
/// ONE file list; a trailing-slash / copy-contents operand contributes its
/// immediate CHILDREN (send_directory, flist.c:2490) rather than its own name,
/// and flist.c:2544 flist_sort_and_clean() then sorts the combined list with
/// f_name_cmp before the generator itemizes it. Two copy-contents sources
/// therefore interleave their contents by child name (files before dirs at each
/// level) instead of being emitted one operand's subtree at a time. This
/// function reproduces that by flattening each source's immediate children into
/// synthetic named operands (each mapping to `dest/<child>` exactly as the child
/// would under upstream), deduplicating by name - a file keeps the first
/// operand's copy (flist_sort_and_clean drops the later duplicate), a directory
/// keeps every contributor so their contents merge - and sorting with the same
/// f_name_cmp comparator via [`compare_file_entries`].
///
/// Scope is deliberately narrow so no other feature's semantics shift: it
/// engages only for a non-`--relative`, recursive, plain copy of >= 2
/// copy-contents sources with no batch writer (whose wire format is built by
/// the per-source walk) and no `--one-file-system` (whose mount-point pruning
/// keys off each source root's device). A fresh destination under `--dry-run`
/// also falls back, since the synthesized `cd ./` root row is emitted from the
/// on-disk root the dry run never creates. Every excluded shape keeps
/// command-line order, exactly as before. A `--delete` run engages too: the
/// destination-root sweep the bypassed whole-root walk would have owned is
/// driven explicitly by [`merged_root_delete_pass`], with the merged entry
/// names as the keep set - mirroring upstream, where the root's
/// `delete_in_dir()` keys off the same combined flist the transfer uses.
fn merged_contents_worklist(
    context: &CopyContext,
    plan: &LocalCopyPlan,
    destination_root_created: bool,
) -> Result<Option<Vec<SourceSpec>>, LocalCopyError> {
    let sources = plan.sources();
    let engaged = sources.len() > 1
        && sources.iter().all(SourceSpec::copy_contents)
        && !context.relative_paths_enabled()
        && context.recursive_enabled()
        && !context.one_file_system_enabled()
        && context.options().get_batch_writer().is_none()
        && !(destination_root_created && context.mode().is_dry_run());
    if !engaged {
        return Ok(None);
    }

    let destination_root = plan.destination_spec().path();
    let mut order: Vec<MergedRootEntry> = Vec::new();
    let mut index: HashMap<OsString, usize> = HashMap::new();

    for source in sources {
        let read_dir = match fs::read_dir(source.path()) {
            Ok(read_dir) => read_dir,
            // A source that is not a readable directory (vanished, a file given a
            // trailing slash, permission denied) cannot be merged. Abandon the
            // merged path so the per-source loop surfaces the exact upstream
            // error / continuation semantics for it.
            Err(_) => return Ok(None),
        };
        for entry in read_dir {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => return Ok(None),
            };
            let path = entry.path();
            // Never descend into our own output: skip a child that IS the
            // destination root (mirrors the per-directory retain in the
            // recursive walk).
            if path == destination_root {
                continue;
            }
            let name = entry.file_name();
            let is_dir = match fs::symlink_metadata(&path) {
                Ok(meta) => meta.file_type().is_dir(),
                Err(_) => return Ok(None),
            };
            match index.get(&name) {
                None => {
                    index.insert(name.clone(), order.len());
                    order.push(MergedRootEntry {
                        is_dir,
                        name,
                        paths: vec![path],
                    });
                }
                Some(&existing) => {
                    let merged = &mut order[existing];
                    // Merge same-named directories (their contents combine under
                    // one dest dir); a file, or a type that disagrees with the
                    // first occurrence, keeps the first operand's entry.
                    if merged.is_dir && is_dir {
                        merged.paths.push(path);
                    }
                }
            }
        }
    }

    // Sort with the generator's f_name_cmp: non-directories before directories,
    // then bytewise name. Contributors to a merged directory keep command-line
    // order so their subtrees recurse in operand order under the shared dest dir.
    order.sort_by(|a, b| compare_file_entries(&merged_sort_entry(a), &merged_sort_entry(b)));

    let mut worklist = Vec::new();
    for entry in order {
        for path in entry.paths {
            worklist.push(SourceSpec::from_child_path(path));
        }
    }
    Ok(Some(worklist))
}

/// Models a merged root entry as a flist entry for f_name_cmp ordering.
fn merged_sort_entry(entry: &MergedRootEntry) -> FileEntry {
    let name = PathBuf::from(&entry.name);
    if entry.is_dir {
        FileEntry::new_directory(name, 0o755)
    } else {
        FileEntry::new_file(name, 0, 0o644)
    }
}

/// Depth bound for the cross-source overlap pre-scan, mirroring the recursive
/// executor's own nesting cap so a pathological (e.g. bind-mount-looped) tree
/// cannot recurse unboundedly.
const CROSS_SCAN_MAX_DEPTH: usize = 1000;

/// Builds the cross-source keep map for a multi-source `--delete` transfer:
/// for each destination directory that MORE THAN ONE source operand
/// contributes to, the union of every contributor's entry names.
///
/// upstream: flist.c:2499 send_file_list() accumulates every source operand
/// into ONE flist, so a `delete_in_dir()` sweep (generator.c:1924-1927 during,
/// generator.c:364-396 do_delete_pass for before/after) can never remove an
/// entry any operand supplies, in any operand order. oc walks each source
/// live; this pre-scan reproduces the shared-flist invariant so a sweep run
/// from one operand's walk protects every sibling operand's entries. Only the
/// OVERLAP is scanned: the destination root (every operand contributes there)
/// and destination directories fed by >= 2 source directories (a trailing-
/// slash operand's subdir shared with another operand's same-named subdir, or
/// same-named named-directory operands). A directory only one source feeds
/// needs no cross protection - its own walk's keep list is already complete.
///
/// Names are recorded as raw (filter-unaware) enumerations; the sweep still
/// applies delete filters to each destination candidate, so the map can only
/// widen protection, never delete more. Unreadable directories are skipped
/// here - the transfer walk surfaces their errors, and an I/O error blocks
/// the delete pass anyway (generator.c:304-311).
fn build_cross_source_keep(
    context: &CopyContext,
    plan: &LocalCopyPlan,
    destination_root: &Path,
) -> HashMap<PathBuf, Vec<OsString>> {
    let mut map = HashMap::new();
    let mut root_names: Vec<OsString> = Vec::new();
    let mut seen_root: HashSet<OsString> = HashSet::new();
    // Destination-root child name -> the source DIRECTORIES that feed it.
    let mut groups: HashMap<OsString, Vec<PathBuf>> = HashMap::new();

    for source in plan.sources() {
        if source.copy_contents() {
            let Ok(read_dir) = fs::read_dir(source.path()) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path == destination_root {
                    continue;
                }
                let name =
                    transcode_filename_component(&entry.file_name(), context.options().iconv())
                        .into_owned();
                if seen_root.insert(name.clone()) {
                    root_names.push(name.clone());
                }
                // lstat semantics: a symlink to a directory is not a directory
                // (its own name is the entry), matching the walker.
                if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_dir()) {
                    groups.entry(name).or_default().push(path);
                }
            }
        } else if let Some(name) = source.path().file_name() {
            let name = transcode_filename_component(name, context.options().iconv()).into_owned();
            if seen_root.insert(name.clone()) {
                root_names.push(name.clone());
            }
            if fs::symlink_metadata(source.path()).is_ok_and(|meta| meta.file_type().is_dir()) {
                groups
                    .entry(name)
                    .or_default()
                    .push(source.path().to_path_buf());
            }
        }
    }

    for (name, contributors) in groups {
        if contributors.len() >= 2 {
            scan_overlap_directory(
                context,
                &mut map,
                destination_root.join(Path::new(&name)),
                &contributors,
                1,
            );
        }
    }
    map.insert(destination_root.to_path_buf(), root_names);
    map
}

/// Records the union of `contributors`' entry names under `dest_dir` and
/// recurses into subdirectories that >= 2 contributors share. See
/// [`build_cross_source_keep`].
fn scan_overlap_directory(
    context: &CopyContext,
    map: &mut HashMap<PathBuf, Vec<OsString>>,
    dest_dir: PathBuf,
    contributors: &[PathBuf],
    depth: usize,
) {
    if depth > CROSS_SCAN_MAX_DEPTH {
        return;
    }
    let mut names: Vec<OsString> = Vec::new();
    let mut seen: HashSet<OsString> = HashSet::new();
    let mut child_groups: HashMap<OsString, Vec<PathBuf>> = HashMap::new();
    for dir in contributors {
        let Ok(read_dir) = fs::read_dir(dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let name = transcode_filename_component(&entry.file_name(), context.options().iconv())
                .into_owned();
            if seen.insert(name.clone()) {
                names.push(name.clone());
            }
            let path = entry.path();
            if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_dir()) {
                child_groups.entry(name).or_default().push(path);
            }
        }
    }
    for (name, group) in child_groups {
        if group.len() >= 2 {
            scan_overlap_directory(
                context,
                map,
                dest_dir.join(Path::new(&name)),
                &group,
                depth + 1,
            );
        }
    }
    map.insert(dest_dir, names);
}

/// Runs (or defers) the destination-root delete sweep for the merged
/// copy-contents work list, which bypasses the per-source whole-root walk that
/// would otherwise own it.
///
/// upstream: the root directory of a trailing-slash transfer is the flist's
/// "." entry; `--delete-before` sweeps it in do_delete_pass()
/// (generator.c:364-396) and `--delete`/`--delete-during` in the generator's
/// first directory visit (generator.c:1924-1927) - both BEFORE the root's
/// children transfer, and both keyed off the combined flist. `--delete-after`
/// and `--delete-delay` sweep it after the transfers (generator.c:2901-2902).
/// The keep set is the merged work list's entry names - the same names the
/// combined flist would carry for the root.
fn merged_root_delete_pass(
    context: &mut CopyContext,
    destination_path: &Path,
    worklist: &[SourceSpec],
    timing: DeleteTiming,
) -> Result<(), LocalCopyError> {
    let mut names: Vec<OsString> = Vec::new();
    let mut seen: HashSet<OsString> = HashSet::new();
    for entry in worklist {
        if let Some(name) = entry.path().file_name() {
            let name = transcode_filename_component(name, context.options().iconv()).into_owned();
            if seen.insert(name.clone()) {
                names.push(name);
            }
        }
    }
    match timing {
        DeleteTiming::Before | DeleteTiming::During => {
            // upstream: generator.c:304-311 - an I/O error suppresses the
            // delete pass unless --ignore-errors; the sweep marker keeps a
            // later frame from re-sweeping the root.
            if !context.delete_pass_blocked_by_io_error()
                && context.mark_directory_swept(destination_path)
            {
                delete_extraneous_entries(context, destination_path, None, &names)?;
            }
        }
        DeleteTiming::After | DeleteTiming::Delay => {
            context.defer_deletion(destination_path.to_path_buf(), None, names);
        }
    }
    Ok(())
}

/// Accounts for the transfer root "." of a merged copy-contents transfer: it
/// counts the per-source "." entries and emits the single displayed root row.
///
/// The per-source recursive walk owns both for the command-line path (the root
/// frame in recursive/mod.rs); the merged path bypasses that walk at the root,
/// so replay it here so `--stats`, `--list-only`, `-v`, and `-i` all match.
///
/// COUNT: upstream sorts the combined flist WITHOUT removing duplicates
/// (flist.c:2535-2544), so every trailing-slash operand's own "." entry counts
/// toward "Number of files (dir: N)" even though the display deduplicates them.
/// A single dirA/ dirB/ transfer therefore reports two "." dirs; count one per
/// source here. (Duplicate/colliding subtree entries under repeated or
/// overlapping sources are still display-deduplicated by
/// [`merged_contents_worklist`], so their non-deduplicated count is a tracked
/// follow-up; distinct sources - the common case - match exactly.)
///
/// DISPLAY: the generator lists the root "." once (duplicates are display-
/// deduplicated). A freshly created root itemizes `cd+++++++++ ./`
/// (main.c:803-805 FLAG_DIR_CREATED, generator.c:566-572); a pre-existing root
/// emits an unchanged `.d` row shown only under `-vv` / `--list-only` and
/// suppressed under `-i` (generator.c:1480-1483 itemizes the existing "." with
/// no significant flags). The created-dir tally and the `created directory
/// <dest>` notice are already owned by `mark_destination_root_created`.
fn emit_merged_transfer_root(
    context: &mut CopyContext,
    plan: &LocalCopyPlan,
    destination_path: &Path,
    destination_root_created: bool,
) -> Result<(), LocalCopyError> {
    for _ in plan.sources() {
        context.summary_mut().record_directory_total();
    }

    // The displayed "." IS the first operand's source directory (upstream sends
    // each operand's "." into the flist and the display keeps the first), so the
    // listed perms/size/mtime and any `-i` drift are the source root's, not the
    // destination's - mirroring the recursive root frame, which snapshots the
    // source `metadata` and itemizes it against the existing destination.
    let Some(source_root) = plan.sources().first().map(SourceSpec::path) else {
        return Ok(());
    };
    let source_meta = fs::symlink_metadata(source_root)
        .map_err(|error| LocalCopyError::io("inspect source", source_root, error))?;
    let snapshot = LocalCopyMetadata::from_metadata(&source_meta, None);
    let snapshot_len = snapshot.len();
    let dot = PathBuf::from(".");
    let record = if destination_root_created {
        LocalCopyRecord::new(
            dot,
            LocalCopyAction::DirectoryCreated,
            0,
            Some(snapshot_len),
            Duration::default(),
            Some(snapshot),
        )
        .with_creation(true)
    } else {
        // Existing root: itemize the source "." against the on-disk destination
        // root so an unchanged pair yields an all-dot `.d` row (shown only under
        // -vv / --list-only), exactly as the recursive root frame does.
        let dest_meta = fs::symlink_metadata(destination_path)
            .map_err(|error| LocalCopyError::io("inspect destination", destination_path, error))?;
        let change_set = LocalCopyChangeSet::for_existing_directory(
            &source_meta,
            &dest_meta,
            &context.metadata_options(),
            context.omit_dir_times_enabled(),
            false,
            false,
            context.options().modify_window(),
        );
        LocalCopyRecord::new(
            dot,
            LocalCopyAction::MetadataReused,
            0,
            Some(snapshot_len),
            Duration::default(),
            Some(snapshot),
        )
        .with_change_set(change_set)
    };
    context.record(record);
    Ok(())
}

/// Entry point for copying all sources to the destination.
///
/// Sets up the copy context, iterates over sources, and handles deferred
/// operations and error rollback.
pub(crate) fn copy_sources(
    plan: &LocalCopyPlan,
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    handler: Option<&mut dyn LocalCopyRecordHandler>,
) -> Result<CopyOutcome, LocalCopyError> {
    // upstream: main.c become_copy_as_user() - permanently drop to the target
    // uid/gid (setgid/setgroups/setuid) before any receiver file operation. The
    // drop is irreversible by design: the process can never regain root and
    // never retains root's supplementary groups while writing files.
    if let Some(ids) = options.copy_as_ids() {
        ::metadata::become_copy_as_user(ids).map_err(|err| {
            LocalCopyError::io(
                "drop to --copy-as identity",
                plan.destination_spec().path(),
                err,
            )
        })?;
    }

    // upstream: main.c:1843 `starttime = time(NULL)` - the transfer rate span
    // is measured between two whole-second time_t marks, not a fractional clock.
    let run_start_secs = whole_unix_seconds();
    let destination_root = plan.destination_spec().path().to_path_buf();
    let mut context = CopyContext::new(mode, options, handler, destination_root);
    context.set_multi_source(plan.sources().len() > 1);

    // upstream: generator.c:2290-2295 - the generator prints the
    // delta-transmission status once at DEBUG_GTE(FLIST, 1) (first active at
    // -vv), before the per-file generate loop. A local copy renders its name
    // list post-hoc in the CLI, so that notice is emitted there (in
    // `emit_transfer_summary`) to keep it ahead of the list rather than
    // dead-last through the deferred diagnostic flush.

    let result = {
        let context = &mut context;
        (|| -> Result<(), LocalCopyError> {
            let multiple_sources = plan.sources().len() > 1;
            let destination_path = plan.destination_spec().path();
            let mut destination_state = query_destination_state(destination_path)?;
            if context.keep_dirlinks_enabled() && destination_state.symlink_to_dir {
                destination_state.is_dir = true;
            }

            // upstream: main.c:803-808 - the receiver pre-flight-mkdirs the
            // destination root, flags the synthetic "." flist entry with
            // FLAG_DIR_CREATED, and emits `created directory %s\n` when
            // INFO_GTE(NAME, 1) || stdout_format_has_i. Surface the same
            // signal here so the CLI itemize gate (rendered in
            // emit_transfer_summary) and the synthesized `cd+++++++++ ./`
            // root record (rendered by copy_directory_recursive) both fire.
            let mut destination_root_created = false;
            let mkpath = context.mkpath_enabled();
            if plan.destination_spec().force_directory() {
                destination_root_created |= ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                    mkpath,
                )?;
            }

            if multiple_sources {
                destination_root_created |= ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                    mkpath,
                )?;
            }

            // upstream: main.c:787 get_local_name() - `if (file_total > 1 ||
            // trailing_slash) { do_mkdir(dest_path); ... }`. The transfer-level
            // decision is made ONCE, from the flist entry count. For a single
            // no-trailing-slash directory source `file_total > 1` requires the
            // directory's children to be enumerated into the flist, which only
            // happens under recursion (`-r`/`-a`): then pre-create the
            // destination root here (counting it and emitting `created directory
            // <dest>`) and let `destination_behaves_like_directory` keep the
            // source name. Without `-r`/`-d` the directory operand is skipped
            // entirely (`flist.c:2451` `!xfer_dirs`) and no destination is
            // created; with `-d` alone a no-trailing-slash directory contributes
            // only its own entry (`file_total == 1`), so the name is dropped and
            // the destination is materialised AS the directory. Gating on
            // recursion keeps all three cases byte-for-byte with upstream.
            if !multiple_sources
                && context.recursive_enabled()
                && !plan.destination_spec().force_directory()
                && !destination_state.is_dir
                && let Some(source) = plan.sources().first()
                && !source.copy_contents()
                && fs::symlink_metadata(source.path()).is_ok_and(|meta| meta.is_dir())
                && fs::read_dir(source.path()).is_ok_and(|mut entries| entries.next().is_some())
            {
                // A pre-existing non-directory destination blocks the mkdir, so
                // clear it first and let the source name land as
                // `dest/<source>/`. upstream: generator.c:1839-1842
                // recv_generator() removes it with `delete_item(fname, ..,
                // del_opts | DEL_FOR_DIR)` regardless of `--force`, which
                // contributes only DEL_RECURSE (generator.c:1629) and so
                // governs recursing into a non-empty *directory*, not this.
                if destination_state.exists && !destination_state.is_dir {
                    if !context.mode().is_dry_run() {
                        remove_existing_destination(destination_path)?;
                    }
                    destination_state.exists = false;
                }
                destination_root_created |= ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                    mkpath,
                )?;
            }

            // upstream: main.c:802-808 - the pre-flight mkdir always prints
            // `created directory <dest>`, but only sets FLAG_DIR_CREATED on the
            // flist top entry (and thus counts the root as a created dir) when
            // that entry's basename is "." - i.e. a copy-contents transfer whose
            // implied root maps to the destination. A single-file or
            // no-trailing-slash directory source has a NAMED top entry
            // ("a.txt"/"src"), so upstream emits the notice yet never counts the
            // destination root; the named entries are counted on their own.
            // Under `--relative` the root is the mkpath target, absent from the
            // flist, counted instead by `emit_relative_implied_parents`; a
            // `./`-anchored operand's "." is accounted for there too. Set the
            // notice flag either way; count the root only for a non-relative
            // copy-contents "." top entry (multiple sources keep their tally).
            if destination_root_created {
                let count_root = !context.relative_paths_enabled()
                    && (multiple_sources
                        || plan.sources().first().is_some_and(|s| s.copy_contents()));
                context
                    .summary_mut()
                    .mark_destination_root_created(count_root);
            }

            let destination_behaves_like_directory =
                destination_state.is_dir || plan.destination_spec().force_directory();

            // upstream: flist.c:2499 send_file_list() folds every operand into
            // ONE flist before the generator's delete passes run, so no sweep
            // can remove an entry a sibling operand supplies. Reproduce that
            // shared-flist invariant for the live-walk engine by installing
            // the cross-source keep map before any operand is walked. With the
            // map ready, multi-source `--delete`/`--delete-during` sweeps run
            // at the upstream point in the walk instead of being downgraded to
            // a deferred pass (see `effective_delete_timing`). `--relative`
            // operand shapes carry implied parent directories the pre-scan
            // does not model; they keep the legacy deferred behaviour.
            if multiple_sources
                && context.options().delete_timing().is_some()
                && !context.relative_paths_enabled()
            {
                let cross = build_cross_source_keep(context, plan, destination_path);
                context.set_cross_source_keep(cross);
            }

            // Build the ordered work list. Upstream accumulates every source into
            // ONE file list and sorts it globally (flist.c:2544
            // flist_sort_and_clean) before the generator itemizes it, so the
            // observable order is name-sorted, never command-line order. For a
            // multi-source copy-contents transfer that means the sources' contents
            // MERGE and sort together at the destination root
            // (`merged_contents_worklist`); for named operands it means the
            // operands themselves sort (`ordered_operands`). Both fall back to
            // command-line order for the shapes they do not cover.
            let worklist: Vec<SourceSpec> =
                match merged_contents_worklist(context, plan, destination_root_created)? {
                    Some(entries) => {
                        // The merged path bypasses the root recursive walk, so
                        // the root's delete sweep and the transfer root "."
                        // accounting (count + display row) are driven here.
                        // The sweep comes first: upstream prints a directory's
                        // `*deleting` rows ahead of its own row
                        // (generator.c:1924-1927 delete_in_dir runs while the
                        // directory entry itself is itemized).
                        if let Some(timing) = context.options().delete_timing() {
                            merged_root_delete_pass(context, destination_path, &entries, timing)?;
                        }
                        emit_merged_transfer_root(
                            context,
                            plan,
                            destination_path,
                            destination_root_created,
                        )?;
                        entries
                    }
                    None => ordered_operands(plan.sources(), context.relative_paths_enabled())
                        .into_iter()
                        .cloned()
                        .collect(),
                };

            let mut first_io_error: Option<LocalCopyError> = None;
            for source in &worklist {
                context.set_source_anchor(source_confinement_anchor(source));
                let result = process_single_source(
                    context,
                    plan,
                    source,
                    destination_path,
                    destination_behaves_like_directory,
                    multiple_sources,
                    destination_root_created,
                );
                if let Err(error) = result {
                    if error.is_vanished_error() {
                        // upstream: flist.c:1317 - vanished files produce a warning
                        // and set IOERR_VANISHED, but transfer continues.
                        // full_fname() wraps the path in double quotes (util1.c:1228).
                        eprintln!("file has vanished: \"{}\"", source.path().display());
                        context.record_io_error();
                        if first_io_error.is_none() {
                            first_io_error = Some(error);
                        }
                    } else if error.is_link_stat_failed() {
                        // upstream: flist.c send_file_list() - a missing source
                        // argument prints `link_stat "%s" failed: %s` to stderr,
                        // sets IOERR_GENERAL (exit 23), and the transfer
                        // continues with the remaining sources.
                        eprintln!("{error}");
                        context.record_io_error();
                        if first_io_error.is_none() {
                            first_io_error = Some(error);
                        }
                    } else if error.is_io_error() {
                        // upstream: rsync continues transferring remaining sources
                        // when individual entries fail with I/O errors, regardless
                        // of whether --delete is active.
                        context.record_io_error();
                        if first_io_error.is_none() {
                            first_io_error = Some(error);
                        }
                    } else {
                        return Err(error);
                    }
                }
            }

            // Write the flist end-of-list marker, ID lists, then delta data.
            // upstream: flist.c:2548-2549 - without INC_RECURSE, send_id_lists()
            // writes uid/gid name mappings after the flist end marker.
            // Since names are already embedded inline via XMIT_USER_NAME_FOLLOWS,
            // the ID lists are empty (just varint30(0) terminators), but they
            // must be present for upstream's recv_id_list() to consume.
            context.finalize_batch_flist()?;
            context.write_batch_id_lists()?;
            context.flush_batch_delta_to_batch()?;
            // Stats are written by core::client::run::batch::finalize_batch()
            // after the engine returns, using actual transfer byte counts.

            // upstream: main.c:1839-1840 - `if (write_batch < 0) dry_run = 1`
            // forces dry_run when `--only-write-batch` is set, so the receiver
            // never reaches do_recv() / finish_transfer(). Mirror that by
            // returning before flushing deferred destination updates: in
            // OnlyWrite mode the batch file is the sole output and no
            // destination-side writes should be performed.
            //
            // The combination `DryRun + batch_writer present` distinguishes
            // `--only-write-batch` (this branch) from plain `--dry-run`
            // (no batch writer; falls through to the deferred-ops flush so
            // empty queues drain cleanly without side effects).
            if context.mode().is_dry_run() && context.options().get_batch_writer().is_some() {
                if let Some(error) = first_io_error {
                    return Err(error);
                }
                if context.iconv_conversion_error_occurred()
                    || context.unsupported_operation_skipped()
                {
                    return Err(LocalCopyError::partial_transfer());
                }
                return Ok(());
            }

            flush_deferred_operations(context)?;

            if let Some(error) = first_io_error {
                return Err(error);
            }
            // A platform-unsupported entry (a Windows unprivileged file symlink)
            // was skipped with a warning; finish RERR_PARTIAL (23) like upstream
            // FERROR_XFER after a failed do_symlink().
            if context.unsupported_operation_skipped() {
                return Err(LocalCopyError::partial_transfer());
            }
            // upstream: flist.c:1631 send_file1() sets io_error |= IOERR_GENERAL
            // when a filename cannot be transcoded under --iconv; main.c:1356
            // then exits RERR_PARTIAL (23). The per-entry diagnostic was already
            // printed at the skip site, so surface only the summary error here.
            if context.iconv_conversion_error_occurred() {
                return Err(LocalCopyError::partial_transfer());
            }
            // upstream: sender.c:successful_send() - a source refused by a
            // --remove-source-files safety guard (changed file / destination
            // inode) or a failed unlink sets got_xfer_error; main.c:1630 then
            // exits RERR_PARTIAL (23). The per-entry diagnostic was already
            // printed at the guard site, so surface only the summary error here.
            if context.sender_remove_error_occurred() {
                return Err(LocalCopyError::partial_transfer());
            }
            // upstream: delete.c:283-285 - a directory obstacle that survived
            // its rmdir logs `could not make way for new %s: %s` at
            // FERROR_XFER, which sets got_xfer_error (log.c:310-311) without
            // aborting; cleanup.c:217-218 then lifts the exit to RERR_PARTIAL
            // (23). The per-entry diagnostics were already printed at the
            // refusal site, so surface only the summary error here.
            if context.make_way_error_occurred() {
                return Err(LocalCopyError::partial_transfer());
            }
            // upstream: sender.c:787-795 - a source that shrank mid-transfer
            // sets `io_error |= IOERR_GENERAL` and logs one `read errors
            // mapping %s` at FERROR_XFER without aborting; main.c then exits
            // RERR_PARTIAL (23). The per-entry diagnostic was already printed
            // at the copy site, so surface only the summary error here.
            if context.source_read_error_occurred() {
                return Err(LocalCopyError::partial_transfer());
            }
            // upstream: delete.c:86-210 - the delete pass logs each
            // un-removable entry via `rsyserr(FERROR_XFER, ...)` and sets
            // `io_error |= IOERR_GENERAL` without aborting; main.c then exits
            // RERR_PARTIAL (23). The per-entry notice was already printed by
            // the emitter, so surface only the summary error here.
            if context.io_error_requires_partial_exit() {
                return Err(LocalCopyError::partial_transfer());
            }
            Ok(())
        })()
    };

    match result {
        Ok(()) => {
            // upstream main.c:327,422: the transfer rate uses `endtime -
            // starttime`, the integer difference of two whole-second time_t
            // marks - not the sum of per-file copy durations (~0 for CoW) and
            // not a fractional clock. Capture the same whole-second span so a
            // transfer crossing a second boundary reports the upstream rate.
            let elapsed_secs = whole_unix_seconds().saturating_sub(run_start_secs);
            context
                .summary_mut()
                .record_wall_clock_elapsed(Duration::from_secs(elapsed_secs));
            Ok(context.into_outcome())
        }
        Err(error) => {
            context.rollback_on_error(&error);
            Err(error)
        }
    }
}

/// Processes a single source entry in the copy operation.
/// The directory the sender would `chdir` into for `source`, used as the
/// confinement anchor for that operand's content reads.
///
/// upstream: `rsync-3.5.0/main.c` chdirs to the transfer root before the
/// sender runs, and `sender.c` opens file content confined beneath it. A
/// trailing-slash operand (`src/`) transfers the directory's *contents*, so the
/// operand itself is the root; otherwise the operand is the first entry and its
/// parent is the root.
///
/// Returns `None` when the operand has no parent (a bare relative name, or
/// `/`), leaving the open to the `O_NOFOLLOW` leaf rule alone rather than
/// anchoring somewhere arbitrary.
///
/// The marker alone does not make the operand a root: upstream's chdir lands
/// only when the name resolves to a DIRECTORY (`change_pathname()` ->
/// `change_dir()`). A marked operand that stats to a file, a symlink to a
/// file, or a dangling symlink keeps its `link_stat` result (`flist.c:292-297`
/// replaces the lstat only for a directory target) and is transferred as an
/// entry under its parent, so the parent is its anchor. The stat follows
/// symlinks because the chdir does.
fn source_confinement_anchor(source: &SourceSpec) -> Option<PathBuf> {
    let path = source.path();
    if source.copy_contents() && fs::metadata(path).is_ok_and(|meta| meta.is_dir()) {
        return Some(path.to_path_buf());
    }
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

fn process_single_source(
    context: &mut CopyContext,
    plan: &LocalCopyPlan,
    source: &SourceSpec,
    destination_path: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
    destination_root_created: bool,
) -> Result<(), LocalCopyError> {
    // Directory copy handlers set the correct offset before recursing.
    context.set_safety_depth_offset(0);
    context.enforce_timeout()?;

    // upstream: flist.c:2652-2657 - the operand's trailing DOTDIR marker is
    // stripped from the name that gets stat'd, opened and read; the marker
    // itself lives on in `name_type` (here `SourceSpec::copy_contents`). The
    // two must stay separate: the raw `operand/` form makes the kernel resolve
    // the trailing slash, which silently converts the lstat into a stat that
    // also demands a directory.
    let operand_path = operand_stat_path(context, source);
    let source_path = operand_path.as_ref();
    let metadata_start = Instant::now();

    let (relative_root, relative_parent) = compute_relative_paths(context, source);

    // upstream: flist.c:3016 flist_sort_and_clean() - after every source arg is
    // merged into one shared flist, an operand that duplicates a subtree already
    // contributed by an earlier `--relative` operand collapses away. The
    // streaming local-copy executor emits each operand's rows as it walks, so a
    // later operand whose SOURCE descends from (or equals) an earlier
    // recursively-walked operand's source directory would re-list the shared
    // subtree - its implied parents, itself, and its recursive contents. Skip it
    // here to reproduce the single-emission result. The covering key is the
    // source filesystem path, not the destination-relative root: two `--relative`
    // operands can carry the same relative suffix from different sources (e.g.
    // `-R down/3/deep extra/./down/3/deep/extra.added.value`), and upstream keeps
    // the distinct file because no earlier flist entry produced it - only a source
    // physically inside an already-walked directory is truly redundant. Guarded on
    // `--relative` (without it operands map to distinct basenames and never
    // overlap in the destination).
    if relative_root.is_some() && context.source_root_already_covered(source_path) {
        return Ok(());
    }

    let metadata_result = fetch_source_metadata(
        context,
        source,
        source_path,
        destination_path,
        destination_behaves_like_directory,
        multiple_sources,
        relative_root.as_deref(),
        metadata_start,
    )?;

    let metadata = match metadata_result {
        SourceMetadataResult::Found(m) => m,
        SourceMetadataResult::Handled => return Ok(()),
        SourceMetadataResult::NotFoundError(error) => {
            // upstream: flist.c send_file_list() - a source argument (operand or
            // --files-from entry) whose link_stat fails is reported as
            // `link_stat "%s" failed`, sets IOERR_GENERAL (exit 23,
            // RERR_PARTIAL), and the transfer continues with the remaining
            // sources. Distinct from a file that vanishes mid-transfer (exit
            // 24, "file has vanished").
            //
            // The name upstream prints is the operand anchored to the working
            // directory, not the operand as typed: `full_fname()` puts
            // `curr_dir` in front of every relative `fn` (util1.c:1445-1452).
            return Err(LocalCopyError::link_stat_failed(
                crate::local_copy::operand_diagnostic_name(source_path),
                error,
            ));
        }
        SourceMetadataResult::IoError(error) => {
            return Err(LocalCopyError::io(
                "access source",
                source_path.to_path_buf(),
                error,
            ));
        }
    };

    context.record_file_list_generation(metadata_start.elapsed());

    // upstream: flist.c:make_file() - skip files with bogus zero st_mode
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() == 0 {
            context.record_io_error();
            return Ok(());
        }
    }

    let file_type = metadata.file_type();

    let destination_base = if let Some(parent) = &relative_parent {
        destination_path.join(parent)
    } else {
        destination_path.to_path_buf()
    };

    let root_device = if context.one_file_system_enabled() {
        device_identifier(source_path, &metadata)
    } else {
        None
    };

    // With -xx (level >= 2), skip root-level source directories that are mount
    // points - i.e. their device ID differs from their parent directory.
    if context.one_file_system_level() >= 2 && file_type.is_dir() {
        if let Some(parent) = source_path.parent() {
            if let Ok(parent_meta) = fs::symlink_metadata(parent) {
                if let Some(parent_dev) = device_identifier(parent, &parent_meta) {
                    if let Some(source_dev) = root_device {
                        if source_dev != parent_dev {
                            let record_relative = relative_root
                                .as_deref()
                                .and_then(|p| non_empty_path(p))
                                .or_else(|| source_path.file_name().map(Path::new));
                            // upstream: flist.c:1347 - INFO_GTE(MOUNT, 1) gates
                            // `rprintf(FINFO, "[%s] skipping mount-point dir %s", who_am_i(),
                            // thisname)` when `-xx` prunes a root-level mount-point source.
                            // The role prefix is added downstream by the renderer.
                            info_log!(
                                Mount,
                                1,
                                "skipping mount-point dir {}",
                                source_path.display()
                            );
                            context.record_skipped_mount_point(record_relative);
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    let proc_ctx = SourceProcessingContext {
        source,
        source_path,
        metadata: metadata.clone(),
        file_type,
        relative_root: relative_root.clone(),
        relative_parent: relative_parent.clone(),
        destination_path,
        destination_base,
        destination_behaves_like_directory,
        multiple_sources,
        root_device,
    };

    let record_relative = proc_ctx.compute_record_relative();
    context.record_file_list_entry(record_relative.as_deref());

    if proc_ctx.requires_directory_destination() && !destination_behaves_like_directory {
        return Err(LocalCopyError::invalid_argument(
            LocalCopyArgumentError::DestinationMustBeDirectory,
        ));
    }

    // upstream: flist.c:2456-2472 - send_file_list() calls send_implied_dirs()
    // for each `--relative` operand *before* send_file_name() emits the operand
    // itself, so the implied parent directories precede the operand's row in the
    // itemize / verbose / stats stream. Surface those ancestor rows here, ahead
    // of the leaf's own record, mirroring that ordering.
    emit_relative_implied_parents(
        context,
        source,
        source_path,
        destination_path,
        relative_root.as_deref(),
        destination_root_created,
    )?;

    // Record this operand's SOURCE path as a covering root once we know it is a
    // directory being walked recursively: its full subtree enters the flist
    // here, so a later `--relative` operand whose source equals or lies beneath
    // it is redundant (see `source_root_already_covered`). Non-recursive walks
    // (`-d`, or a single file) do not cover descendants, so they are not
    // registered. Keyed on the source path (not the relative root) so a distinct
    // source sharing the relative suffix is never wrongly skipped.
    if file_type.is_dir() && context.recursive_enabled() && relative_root.is_some() {
        context.register_expanded_source_root(source_path.to_path_buf());
    }

    if file_type.is_dir() {
        if source.copy_contents() {
            handle_directory_contents_copy(
                context,
                source_path,
                &metadata,
                relative_root.as_ref(),
                destination_path,
                root_device,
            )?;
        } else {
            handle_directory_copy(
                context,
                source_path,
                &metadata,
                relative_root.as_ref(),
                destination_path,
                &proc_ctx.destination_base,
                destination_behaves_like_directory,
                multiple_sources,
                root_device,
            )?;
        }
    } else {
        handle_non_directory_source(context, &proc_ctx, plan)?;
    }

    retouch_relative_implied_dirs(
        context,
        source,
        source_path,
        destination_path,
        relative_root.as_deref(),
    )?;

    context.enforce_timeout()?;
    Ok(())
}

/// Surfaces the implied parent directories along a `--relative` source's path
/// as flist entries so the local-copy itemize (`-i`), `--stats` counters, and
/// verbose (`-v`) listing match upstream. Only the ancestors *above* the leaf
/// are recorded here; the leaf's own row is emitted by the file/directory
/// handler. The directories themselves are still physically materialized by
/// `prepare_parent_directory` during the leaf's copy - this pass records them.
///
/// upstream: flist.c:1937 `send_implied_dirs()` emits one `FLAG_IMPLIED_DIR`
/// entry per ancestor component (`flist.c:1989-1998`), bypassing the filter
/// chain (`flist.c:1950`) and deduplicating shared ancestors across operands;
/// the receiver then itemizes each as `cd+++++++++ <dir>/` and counts it under
/// the created-dir stats. `--no-implied-dirs` clears `implied_dirs`
/// (`flist.c:2468`), suppressing the whole set - mirrored by the guard below.
fn emit_relative_implied_parents(
    context: &mut CopyContext,
    source: &SourceSpec,
    source_path: &Path,
    destination_path: &Path,
    relative_root: Option<&Path>,
    destination_root_created: bool,
) -> Result<(), LocalCopyError> {
    if !context.relative_paths_enabled() {
        return Ok(());
    }
    let Some(relative) = relative_root else {
        return Ok(());
    };

    // upstream: flist.c:2258 - a protocol >= 30 sender (which a local transfer
    // always is, being a proto-32 sender/receiver pair) forces `implied_dirs = 1`
    // so the flagged implied parents are always placed in the shared flist and
    // counted toward "Number of files (dir: N)". `--no-implied-dirs` only tells
    // the receiver not to apply the source dir's attributes, which suppresses
    // the itemize row and the created-dir tally - not the flist entry itself.
    let itemize = context.implied_dirs_enabled();

    let metadata_options = context.metadata_options();
    let omit_dir_times = context.omit_dir_times_enabled();
    let modify_window = context.options().modify_window();

    // Dot-dir transfer root ".": upstream flist.c:2368+2417-2419 injects a
    // synthetic "." entry into the flist only for an operand that *begins* with
    // `./` (`implied_dot_dir`), so it counts toward "Number of files" whenever
    // directories are being transferred (the `xfer_dirs` gate below). A dot in
    // the middle of the path (e.g. `src/./sub`) reroots the relative path but
    // adds no "." entry. `dot_dir_anchor()` returns "." exactly for the leading
    // form (its prefix-skip is zero). The entry is a real flist member, so it is
    // both counted and itemized like any other implied parent; the created-dir
    // bump stays owned by `mark_destination_root_created`, which is what
    // distinguishes the freshly made root from a pre-existing one.
    if source.dot_dir_anchor().as_deref() == Some(Path::new(".")) {
        let dot = PathBuf::from(".");
        if context.mark_implied_dir_emitted(&dot) {
            // upstream: flist.c:2419 routes the synthetic "." through
            // send_file_name() -> make_file(), whose `if (S_ISDIR(st.st_mode))
            // { if (!xfer_dirs) { rprintf(FINFO, "skipping directory %s\n",
            // thisname); return NULL; } }` (flist.c:1336-1340) drops it when
            // directories are not being transferred. `thisname` is the
            // cleaned name passed in, so the text is exactly
            // `skipping directory .`. A NULL from make_file means no flist
            // entry at all: the "." is neither sized into the flist nor
            // counted under "Number of files (dir: N)".
            //
            // upstream: options.c:2197-2203 resolves `xfer_dirs` to
            // `recurse || -d || (list_only when neither was given)`;
            // options.c:2190-2191 forces it on for --files-from, which takes
            // the operand's own path and never reaches this branch.
            let xfer_dirs = context.recursive_enabled()
                || context.dirs_enabled()
                || context.list_only_enabled();
            if xfer_dirs {
                context.record_file_list_entry(non_empty_path(dot.as_path()));
                context.summary_mut().record_directory_total();

                // The leading-dot "." maps to the destination transfer root. When
                // that root is freshly created this run, upstream itemizes it
                // `cd+++++++++ ./` and counts it as a created dir (main.c:803-808);
                // a pre-existing root stays count-only (unchanged, suppressed under
                // -i). `--no-implied-dirs` (itemize == false) drops both. In dry-run
                // the mkdir is elided, so "would create" is inferred from the root's
                // absence on disk. The itemize row snapshots the source anchor dir,
                // which exists in both modes (the destination may not yet).
                let root_created = destination_root_created
                    || (context.mode().is_dry_run() && !destination_path.exists());
                if itemize
                    && let Some(anchor) = source.dot_dir_anchor()
                    && let Ok(meta) = fs::symlink_metadata(&anchor)
                    && meta.file_type().is_dir()
                {
                    let snapshot = LocalCopyMetadata::from_metadata(&meta, None);
                    let snapshot_len = snapshot.len();
                    let record = if root_created {
                        context.summary_mut().record_directory();
                        Some(
                            LocalCopyRecord::new(
                                dot.clone(),
                                LocalCopyAction::DirectoryCreated,
                                0,
                                Some(snapshot_len),
                                Duration::default(),
                                Some(snapshot),
                            )
                            .with_creation(true),
                        )
                    } else if let Ok(existing) = fs::symlink_metadata(destination_path)
                        && existing.file_type().is_dir()
                    {
                        // Pre-existing transfer root: upstream still placed the
                        // synthetic "." in the flist, so the receiver reaches it
                        // like any other implied parent and itemizes it against
                        // the basis - an unchanged root is an all-dot `.d ./`
                        // row, surfaced under -ii and under --list-only and
                        // suppressed at -i. Same treatment the ancestor loop
                        // below gives `sub/`; only the leading "." was missing
                        // it, so the row vanished from both those cells while
                        // `--stats` (fed by record_file_list_entry above) still
                        // counted it.
                        let change_set = LocalCopyChangeSet::for_existing_directory(
                            &meta,
                            &existing,
                            &metadata_options,
                            omit_dir_times,
                            false,
                            false,
                            modify_window,
                        );
                        Some(
                            LocalCopyRecord::new(
                                dot.clone(),
                                LocalCopyAction::MetadataReused,
                                0,
                                Some(snapshot_len),
                                Duration::default(),
                                Some(snapshot),
                            )
                            .with_change_set(change_set),
                        )
                    } else {
                        None
                    };
                    if let Some(record) = record {
                        context.record(record);
                    }
                }
            } else {
                context.record_skipped_directory(non_empty_path(dot.as_path()));
            }
        }
    }

    let components: Vec<&std::ffi::OsStr> = relative.iter().collect();
    let parent_count = components.len().saturating_sub(1);
    if parent_count == 0 {
        return Ok(());
    }

    let Some(source_root) = strip_path_suffix(source_path, relative) else {
        return Ok(());
    };
    let destination_root = strip_path_suffix(destination_path, relative)
        .unwrap_or_else(|| destination_path.to_path_buf());

    let mut accumulated = PathBuf::new();
    for component in &components[..parent_count] {
        accumulated.push(component);

        // Emit each ancestor once per transfer (upstream dedups via lastpath +
        // flist_sort_and_clean); a later operand sharing this prefix must not
        // re-list it.
        if !context.mark_implied_dir_emitted(&accumulated) {
            continue;
        }

        let source_dir = source_root.join(&accumulated);
        // upstream: flist.c:1985 - send_implied_dirs() sets `copy_links =
        // xfer_dirs = 1` around the ancestor loop, so the implied-parent stat
        // FOLLOWS symlinks: a symlinked ancestor is emitted as a real directory
        // (its `-i` itemize row and `--stats` dir count), matching the two
        // protocol paths (generator/file_list/mod.rs:326,491). symlink_metadata
        // reports a symlinked ancestor as a non-directory and drops it, losing
        // the itemize row and the dir count. Best-effort: an ancestor that
        // vanished or is not a directory is skipped silently.
        let source_meta = match fs::metadata(&source_dir) {
            Ok(meta) if meta.file_type().is_dir() => meta,
            _ => continue,
        };
        let destination_dir = destination_root.join(&accumulated);
        let existing_meta = match fs::symlink_metadata(&destination_dir) {
            Ok(meta) if meta.file_type().is_dir() => Some(meta),
            _ => None,
        };

        let relative_path = accumulated.clone();

        // upstream: flist.c:2258 - implied dirs always join the shared flist, so
        // each counts toward "Number of files (dir: N)" even under
        // --no-implied-dirs.
        context.record_file_list_entry(non_empty_path(relative_path.as_path()));
        context.summary_mut().record_directory_total();

        // --no-implied-dirs (itemize == false): the receiver applies no source
        // attributes, so there is no itemize/verbose row and no created-dir
        // bump - the flist count above is the only observable effect.
        if !itemize {
            continue;
        }

        let snapshot = LocalCopyMetadata::from_metadata(&source_meta, None);
        let snapshot_len = snapshot.len();

        let record = if let Some(existing) = existing_meta.as_ref() {
            // Pre-existing ancestor: no ITEM_IS_NEW; itemize against the basis so
            // an unchanged dir stays an all-dot `.d` row (shown only under -vv).
            let change_set = LocalCopyChangeSet::for_existing_directory(
                &source_meta,
                existing,
                &metadata_options,
                omit_dir_times,
                false,
                false,
                modify_window,
            );
            LocalCopyRecord::new(
                relative_path,
                LocalCopyAction::MetadataReused,
                0,
                Some(snapshot_len),
                Duration::default(),
                Some(snapshot),
            )
            .with_change_set(change_set)
        } else {
            context.summary_mut().record_directory();
            LocalCopyRecord::new(
                relative_path,
                LocalCopyAction::DirectoryCreated,
                0,
                Some(snapshot_len),
                Duration::default(),
                Some(snapshot),
            )
            .with_creation(true)
        };
        context.record(record);
    }

    Ok(())
}

/// Retouches the implied parent directories materialized along this source's
/// `--relative` chain so they carry the source's directory metadata rather
/// than the wall-clock timestamps deposited by `create_dir_all` and the FS
/// side-effect of writing children.
///
/// Mirrors upstream rsync's two-phase approach:
///
/// 1. `flist.c:2417-2419` + `flist.c:1948` (`send_implied_dirs`) emit each
///    implied parent (and the leading `.` when the operand carries the dot
///    marker) into the flist with `FLAG_IMPLIED_DIR`.
/// 2. `generator.c:1503` (`set_file_attrs` during `recv_generator`) and
///    `generator.c:2128-2136` (`touch_up_dirs` at end-of-transfer) make sure
///    every implied dir ends the transfer with the source's mtime/perms even
///    after the receiver wrote children into it.
///
/// We replay both stages in a single post-source pass so file children added
/// via a sibling source cannot leave the parent dir's mtime stuck at the
/// wall-clock value the FS assigned during the file write.
fn retouch_relative_implied_dirs(
    context: &mut CopyContext,
    source: &SourceSpec,
    source_path: &Path,
    destination_path: &Path,
    relative_root: Option<&Path>,
) -> Result<(), LocalCopyError> {
    if context.mode().is_dry_run()
        || !context.relative_paths_enabled()
        || !context.implied_dirs_enabled()
    {
        return Ok(());
    }

    let metadata_options = if context.omit_dir_times_enabled() {
        context.metadata_options().preserve_times(false)
    } else {
        context.metadata_options()
    };

    // Phase 1: stamp the destination operand from the dot-dir anchor when the
    // operand carries an explicit `./` marker. Upstream emits this as the
    // synthetic `.` entry in `flist.c:2419`.
    if source.has_dot_dir_marker()
        && let Some(anchor) = source.dot_dir_anchor()
    {
        stamp_directory_from_source(destination_path, &anchor, &metadata_options)?;
    }

    // Phase 2: walk every implied parent dir along the relative chain and
    // stamp each one from its source counterpart. For directory sources we
    // skip the leaf because copy_directory_recursive's own
    // apply_final_directory_metadata stamps it; for file/symlink/special
    // sources every component of the relative path IS an implied parent.
    let Some(relative) = relative_root else {
        return Ok(());
    };
    let components: Vec<&std::ffi::OsStr> = relative.iter().collect();
    if components.is_empty() {
        return Ok(());
    }
    let parent_count = components.len().saturating_sub(1);
    if parent_count == 0 {
        return Ok(());
    }

    let Some(source_root) = strip_path_suffix(source_path, relative) else {
        return Ok(());
    };
    let destination_root = strip_path_suffix(destination_path, relative)
        .unwrap_or_else(|| destination_path.to_path_buf());

    let mut accumulated = PathBuf::new();
    for component in &components[..parent_count] {
        accumulated.push(component);
        let src_dir = source_root.join(&accumulated);
        let dst_dir = destination_root.join(&accumulated);
        stamp_directory_from_source(&dst_dir, &src_dir, &metadata_options)?;
    }

    Ok(())
}

/// Applies `source_dir`'s directory metadata to `dest_dir`, silently skipping
/// the pair when either side is not a directory we can stat. Mirrors the
/// best-effort stance of upstream `set_file_attrs` for implied dirs - the
/// transfer is allowed to proceed even when an implied parent is unstable
/// (vanished or replaced with a non-dir between phases).
fn stamp_directory_from_source(
    dest_dir: &Path,
    source_dir: &Path,
    metadata_options: &::metadata::MetadataOptions,
) -> Result<(), LocalCopyError> {
    let source_meta = match fs::symlink_metadata(source_dir) {
        Ok(meta) if meta.file_type().is_dir() => meta,
        _ => return Ok(()),
    };
    let dest_meta = match fs::symlink_metadata(dest_dir) {
        Ok(meta) if meta.file_type().is_dir() => meta,
        _ => return Ok(()),
    };
    // The implied parent already exists on disk here, so its current stat is
    // the `dest_mode()` exists input: a `--chmod` without `--perms` keeps
    // its bits rather than rewriting them.
    ::metadata::apply_directory_metadata_with_options(
        dest_dir,
        &source_meta,
        metadata_options.clone(),
        Some(&dest_meta),
    )
    .map_err(crate::local_copy::map_metadata_error)?;
    Ok(())
}

/// Strips `relative` from the trailing components of `path`. Returns the
/// remaining prefix. Used to recover the source/destination roots used to
/// join an implied-dir chain back together for stamping.
fn strip_path_suffix(path: &Path, relative: &Path) -> Option<PathBuf> {
    let path_components: Vec<_> = path.components().collect();
    let rel_components: Vec<_> = relative.components().collect();
    if rel_components.len() > path_components.len() {
        return None;
    }
    let split = path_components.len() - rel_components.len();
    for (idx, rel) in rel_components.iter().enumerate() {
        if path_components[split + idx].as_os_str() != rel.as_os_str() {
            return None;
        }
    }
    let mut root = PathBuf::new();
    for component in &path_components[..split] {
        root.push(component.as_os_str());
    }
    if root.as_os_str().is_empty() {
        return Some(PathBuf::from("."));
    }
    Some(root)
}

/// Flushes all deferred operations after source processing is complete.
fn flush_deferred_operations(context: &mut CopyContext) -> Result<(), LocalCopyError> {
    context.flush_deferred_updates()?;
    context.flush_deferred_deletions()?;
    context.flush_deferred_syncs()?;
    // Final directory-mtime touch-up. Runs once, after every late in-directory
    // mutation above (delayed-update renames, deletions, backups) has bumped
    // the destination directory mtimes that apply_final_directory_metadata set.
    // upstream: generator.c:2449-2451 touch_up_dirs after handle_delayed_updates.
    context.touch_up_dirs();
    context.enforce_timeout()?;
    Ok(())
}

/// Deletes a destination entry whose source has gone missing.
///
/// Invoked when `--delete-missing-args` is active and the source path
/// no longer exists on disk.
pub(super) fn delete_missing_source_entry(
    context: &mut CopyContext,
    source: &SourceSpec,
    destination_path: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
    relative_root: Option<&Path>,
) -> Result<(), LocalCopyError> {
    if source.copy_contents() {
        return Ok(());
    }

    let source_path = source.path();
    let relative = if let Some(root) = relative_root {
        root.to_path_buf()
    } else {
        let name = source_path.file_name().ok_or_else(|| {
            LocalCopyError::invalid_argument(LocalCopyArgumentError::FileNameUnavailable)
        })?;
        PathBuf::from(Path::new(name))
    };

    let target = if destination_behaves_like_directory || multiple_sources {
        destination_path.join(&relative)
    } else {
        destination_path.to_path_buf()
    };

    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect destination entry",
                target.clone(),
                error,
            ));
        }
    };

    let file_type = metadata.file_type();

    if !context.allows_deletion(relative.as_path(), file_type.is_dir()) {
        return Ok(());
    }

    if let Some(limit) = context.options().max_deletion_limit()
        && context.summary().items_deleted() >= limit
    {
        return Err(LocalCopyError::delete_limit_exceeded(1));
    }

    let record_path = non_empty_path(relative.as_path());

    if context.mode().is_dry_run() {
        context.summary_mut().record_deletion(file_type);
        if let Some(path) = record_path {
            context.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::EntryDeleted,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
        context.register_progress();
        return Ok(());
    }

    // upstream: delete.c:167 - prefer_rename=True; the item is unlinked
    // outright right after, so skip the hard-link tier.
    context.backup_existing_entry(&target, record_path, file_type, true)?;
    let removal = if file_type.is_dir() {
        fs::remove_dir_all(&target)
    } else {
        fs::remove_file(&target)
    };

    match removal {
        Ok(()) => {
            context.summary_mut().record_deletion(file_type);
            if let Some(path) = record_path {
                context.record(LocalCopyRecord::new(
                    path.to_path_buf(),
                    LocalCopyAction::EntryDeleted,
                    0,
                    None,
                    Duration::default(),
                    None,
                ));
            }
            context.register_progress();
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let action = if file_type.is_dir() {
                "remove destination directory"
            } else {
                "remove destination entry"
            };
            return Err(LocalCopyError::io(action, target, error));
        }
    }

    Ok(())
}
