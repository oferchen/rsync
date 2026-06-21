//! Hard-link deduplication and reference directory (`--link-dest`, `--copy-dest`,
//! `--compare-dest`) handling for file copies.
//!
//! Attempts to satisfy a file transfer via hard link or reference copy before
//! falling back to a full data transfer.
//!
//! upstream: generator.c - hard_link_one(), do_hard_links logic
//! upstream: generator.c - find_fuzzy(), compare_dest handling

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use logging::info_log;

use crate::local_copy::overrides::create_hard_link;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet, LocalCopyError,
    LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord, ReferenceDecision, ReferenceQuery,
    find_reference_action, map_metadata_error, remove_source_entry_if_requested,
};

#[cfg(all(any(unix, windows), feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;

use ::metadata::MetadataOptions;
use ::metadata::apply_file_metadata_with_options;

use super::super::super::super::CROSS_DEVICE_ERROR_CODE;
use super::super::guard::remove_existing_destination;

/// Returns `true` when `destination` is already hardlinked to `target`.
///
/// On Unix this compares (device, inode) pairs. On non-Unix platforms
/// (where inode-level inspection is unavailable) the function always
/// returns `false`, causing the link to be re-created harmlessly.
///
/// upstream: hlink.c - `hard_link_check()` skips re-creation when the
/// destination inode already matches the source group leader.
fn is_already_hardlinked(destination: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(dest_meta) = fs::symlink_metadata(destination) else {
            return false;
        };
        let Ok(target_meta) = fs::symlink_metadata(target) else {
            return false;
        };
        dest_meta.dev() == target_meta.dev() && dest_meta.ino() == target_meta.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (destination, target);
        false
    }
}

/// Result of hard-link and reference-directory processing for a file.
pub(super) struct LinkOutcome {
    pub(super) copy_source_override: Option<PathBuf>,
    /// When set, the pending copy reconstructs the file from this
    /// `--copy-dest` basis and must itemize as a local change (`c`) compared
    /// against the basis rather than as a network transfer (`>`).
    pub(super) reference_basis: Option<PathBuf>,
    pub(super) completed: bool,
}

/// Attempts hard-link deduplication and reference directory lookups before
/// falling back to a full file copy.
///
/// Returns `completed: true` when the file was fully handled (linked or
/// skipped), or `completed: false` with an optional `copy_source_override`
/// when the caller should proceed with a data copy.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_links(
    context: &mut CopyContext<'_>,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    record_path: &Path,
    relative_for_link: &Path,
    metadata_options: MetadataOptions,
    existing_metadata: Option<&fs::Metadata>,
    destination_previously_existed: bool,
    file_type: fs::FileType,
    size_only_enabled: bool,
    ignore_times_enabled: bool,
    checksum_enabled: bool,
    mode: LocalCopyExecution,
    #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
    #[cfg(all(any(unix, windows), feature = "acl"))] preserve_acls: bool,
) -> Result<LinkOutcome, LocalCopyError> {
    #[cfg(not(all(unix, any(feature = "xattr", feature = "acl"))))]
    let _ = mode;

    let mut copy_source_override: Option<PathBuf> = None;
    let mut reference_basis: Option<PathBuf> = None;

    // 1. --link-dest style linking
    if let Some(link_target) = context.link_dest_target(
        relative_for_link,
        source,
        metadata,
        size_only_enabled,
        ignore_times_enabled,
        checksum_enabled,
    )? {
        let mut attempted_commit = false;
        loop {
            match fast_io::hard_link(&link_target, destination) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_existing_destination(destination)?;
                    fast_io::hard_link(&link_target, destination).map_err(|link_error| {
                        LocalCopyError::io(
                            "create hard link",
                            destination.to_path_buf(),
                            link_error,
                        )
                    })?;
                    break;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && context.delay_updates_enabled()
                        && !attempted_commit =>
                {
                    context.commit_deferred_update_for(&link_target)?;
                    attempted_commit = true;
                    continue;
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "create hard link",
                        destination.to_path_buf(),
                        error,
                    ));
                }
            }
        }

        // upstream: hlink.c:236 - rprintf(code, "%s => %s\n", fname, realname)
        // emitted at INFO_GTE(NAME, 1) when a hard link is created via --link-dest.
        info_log!(
            Name,
            1,
            "{} => {}",
            record_path.display(),
            link_target.display()
        );

        // upstream: hlink.c:215-224 / generator.c:1008-1013 - a `--link-dest`
        // cluster member is linked from the basis, so it ends up sharing the
        // same inode as its already-placed in-transfer leader. `maybe_hard_link`
        // then takes the same-inode branch, itemizing with an empty xname and
        // emitting `"%s is uptodate"` at NAME>=2: the row is suppressed at plain
        // `-i` but shown blank with a `=> leader` trailer under `-vv`. Thread the
        // leader through the symlink_target slot for the `%L` trailer and flag
        // the record uptodate so the plain-`-i` gate drops it.
        let leader_display = context
            .existing_hard_link_target(metadata)
            .and_then(|leader| {
                leader
                    .strip_prefix(context.destination_root())
                    .map(std::path::Path::to_path_buf)
                    .ok()
                    .filter(|relative| relative != record_path)
            });
        context.record_hard_link(metadata, destination);
        context.summary_mut().record_hard_link();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, leader_display);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(
            LocalCopyRecord::new(
                record_path.to_path_buf(),
                LocalCopyAction::HardLink,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            )
            // A `--link-dest` hardlink reproduces the basis file exactly, so it
            // is reported as `"<path> is uptodate"` under `-vv` and suppressed at
            // plain `-i` - the same gate as an already-shared-inode alias.
            .with_hardlink_uptodate(true),
        );
        context.register_created_path(
            destination,
            CreatedEntryKind::HardLink,
            destination_previously_existed,
        );
        remove_source_entry_if_requested(context, source, Some(record_path), file_type)?;
        return Ok(LinkOutcome {
            copy_source_override: None,
            reference_basis: None,
            completed: true,
        });
    }

    // 2. link to an already-copied inode we cached
    if let Some(existing_target) = context.existing_hard_link_target(metadata) {
        // upstream: hlink.c - hard_link_check() skips re-creation when the
        // destination already shares the same inode as the group leader.
        // Without this check, an up-to-date hardlink emits `hf+++++++++`
        // and unnecessarily removes + re-creates the destination entry.
        if is_already_hardlinked(destination, &existing_target) {
            if let Some(existing) = existing_metadata {
                ::metadata::apply_file_metadata_if_changed(
                    destination,
                    metadata,
                    existing,
                    &metadata_options,
                )
                .map_err(map_metadata_error)?;
            }
            #[cfg(all(unix, feature = "xattr"))]
            sync_xattrs_if_requested(
                preserve_xattrs,
                mode,
                source,
                destination,
                true,
                context.filter_program(),
            )?;
            #[cfg(all(any(unix, windows), feature = "acl"))]
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
            context.record_hard_link(metadata, destination);
            context.summary_mut().record_regular_file_matched();
            let total_bytes = Some(metadata.len());
            let mut change_set = LocalCopyChangeSet::for_file(
                metadata,
                existing_metadata,
                &metadata_options,
                true,
                false,
                {
                    #[cfg(all(unix, feature = "xattr"))]
                    {
                        preserve_xattrs
                    }
                    #[cfg(not(all(unix, feature = "xattr")))]
                    {
                        false
                    }
                },
                {
                    #[cfg(all(any(unix, windows), feature = "acl"))]
                    {
                        preserve_acls
                    }
                    #[cfg(not(all(any(unix, windows), feature = "acl")))]
                    {
                        false
                    }
                },
            );
            // upstream: hlink.c:218-222 + generator.c:528-530 - the hardlink
            // leader-reuse path emits `itemize(..., ITEM_LOCAL_CHANGE, ...)`,
            // and ITEM_REPORT_TIME fires whenever the leader's source mtime
            // differs from the existing destination alias's mtime, even
            // without `-t` (`preserve_mtimes`). Without `-t` upstream's
            // glyph is `T` (TransferTime).
            if !metadata_options.times()
                && let Some(existing) = existing_metadata
                && metadata.modified().ok() != existing.modified().ok()
            {
                change_set =
                    change_set.with_time_change(Some(crate::local_copy::TimeChange::TransferTime));
            }
            // upstream: hlink.c:218-222 - when the destination already
            // shares the source group leader's inode, `maybe_hard_link()`
            // calls `itemize(fname, file, ndx, statret, sxp,
            // ITEM_LOCAL_CHANGE | ITEM_XNAME_FOLLOWS, 0, "")` with an
            // empty xname. `log.c:643-654` skips the `%L` `=> %s` suffix
            // when the xname is empty, so the upstream `-i` row for an
            // already-linked alias is `hf<dots>` with no trailer (line
            // 122 of `testsuite/itemize.test`: `hf$allspace foo/extra`).
            // Pass `None` for the symlink_target so the `%L` placeholder
            // renders to the empty string, matching that behaviour.
            //
            // Tagging the record as HardLink keeps position 0 at 'h'
            // instead of '.'; omitting `.with_creation(true)` keeps
            // positions 2-10 as deltas instead of `+++++++++`.
            let reuse_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            context.record(
                LocalCopyRecord::new(
                    record_path.to_path_buf(),
                    LocalCopyAction::HardLink,
                    0,
                    total_bytes,
                    Duration::default(),
                    Some(reuse_snapshot),
                )
                .with_change_set(change_set)
                // upstream: hlink.c:218-224 - when the destination already
                // shares the source group leader's inode, the generator
                // emits `"%s is uptodate"` at INFO_GTE(NAME, 2). Mark the
                // record so the CLI verbose renderer prints that suffix
                // instead of the bare path.
                .with_hardlink_uptodate(true),
            );
            remove_source_entry_if_requested(context, source, Some(record_path), file_type)?;
            return Ok(LinkOutcome {
                copy_source_override: None,
                reference_basis: None,
                completed: true,
            });
        }

        let mut attempted_commit = false;
        loop {
            match create_hard_link(&existing_target, destination) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_existing_destination(destination)?;
                    create_hard_link(&existing_target, destination).map_err(|link_error| {
                        LocalCopyError::io(
                            "create hard link",
                            destination.to_path_buf(),
                            link_error,
                        )
                    })?;
                    break;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && context.delay_updates_enabled()
                        && !attempted_commit =>
                {
                    context.commit_deferred_update_for(&existing_target)?;
                    attempted_commit = true;
                    continue;
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "create hard link",
                        destination.to_path_buf(),
                        error,
                    ));
                }
            }
        }

        // upstream: hlink.c:236 - "%s => %s" at INFO_GTE(NAME, 1) when an
        // already-copied inode is hard-linked into place.
        info_log!(
            Name,
            1,
            "{} => {}",
            record_path.display(),
            existing_target.display()
        );
        // upstream: hlink.c:223 - "%s is uptodate" at INFO_GTE(NAME, 2) when
        // the destination's content is already in sync with the source group
        // leader's inode. Upstream emits both the `=> realname` arrow (NAME>=1)
        // and the `is uptodate` notice (NAME>=2) for the same alias when the
        // generator pipes the row through `maybe_hard_link()` against a matched
        // basis. Mirror that pairing here so `-vv` reports the leader-uptodate
        // status alongside the freshly-linked alias.
        info_log!(Name, 2, "{} is uptodate", record_path.display());

        context.record_hard_link(metadata, destination);
        context.summary_mut().record_hard_link();
        // upstream: hlink.c:237 + log.c:643-654 - thread the leader's
        // relative path through the snapshot's `symlink_target` slot so the
        // `%L` placeholder emits the upstream `=> %s` suffix. Strip the
        // destination root so the path is relative.
        let link_target_display = existing_target
            .strip_prefix(context.destination_root())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|_| existing_target.clone());
        let metadata_snapshot =
            LocalCopyMetadata::from_metadata(metadata, Some(link_target_display));
        let total_bytes = Some(metadata_snapshot.len());
        // upstream: hlink.c:218-222 - `itemize(..., ITEM_LOCAL_CHANGE, ...)`
        // is emitted whether the alias was created from scratch or the
        // existing inode was reused. Build a change_set so the attribute
        // slots reflect the real deltas (mtime / perms / etc.) instead of
        // collapsing to all-dots-or-spaces.
        //
        // upstream: generator.c:1009-1013 - the itemize for a hardlinked alias
        // is computed with `statret = 1` against the group leader's stat
        // (`sxp->st`), not a (possibly absent) prior destination. Comparing the
        // source against the leader (`existing_target`) keeps the size/time/perm
        // columns blank when the alias is identical to its leader, even though
        // the alias path itself is being created this run. Fall back to the
        // prior-destination comparison when the leader stat is unavailable.
        let leader_metadata = fs::symlink_metadata(&existing_target).ok();
        let (compare_against, compare_existed) = match leader_metadata.as_ref() {
            Some(leader) => (Some(leader), true),
            None => (existing_metadata, destination_previously_existed),
        };
        let mut change_set = LocalCopyChangeSet::for_file(
            metadata,
            compare_against,
            &metadata_options,
            compare_existed,
            false,
            {
                #[cfg(all(unix, feature = "xattr"))]
                {
                    preserve_xattrs
                }
                #[cfg(not(all(unix, feature = "xattr")))]
                {
                    false
                }
            },
            {
                #[cfg(all(any(unix, windows), feature = "acl"))]
                {
                    preserve_acls
                }
                #[cfg(not(all(any(unix, windows), feature = "acl")))]
                {
                    false
                }
            },
        );
        // upstream: generator.c:528-530 - ITEM_REPORT_TIME fires for the
        // hardlink-create path when the source mtime differs from the
        // existing destination alias, even without `-t`. Use `TransferTime`
        // (`T`) to mirror the `testsuite/itemize.test` golden at line 78.
        if !metadata_options.times()
            && let Some(existing) = existing_metadata
            && metadata.modified().ok() != existing.modified().ok()
        {
            change_set =
                change_set.with_time_change(Some(crate::local_copy::TimeChange::TransferTime));
        }
        context.record(
            LocalCopyRecord::new(
                record_path.to_path_buf(),
                LocalCopyAction::HardLink,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            )
            .with_change_set(change_set),
        );
        context.register_created_path(
            destination,
            CreatedEntryKind::HardLink,
            destination_previously_existed,
        );
        remove_source_entry_if_requested(context, source, Some(record_path), file_type)?;
        return Ok(LinkOutcome {
            copy_source_override: None,
            reference_basis: None,
            completed: true,
        });
    }

    // 3. reference directory lookup
    if !context.reference_directories().is_empty()
        && !record_path.as_os_str().is_empty()
        && let Some(decision) = find_reference_action(
            context,
            ReferenceQuery {
                destination,
                relative: record_path,
                source,
                metadata,
                size_only: size_only_enabled,
                ignore_times: ignore_times_enabled,
                checksum: checksum_enabled,
            },
        )?
    {
        match decision {
            ReferenceDecision::Skip(basis) => {
                // upstream: generator.c:1010,1133 / rsync.c:676 - "is uptodate"
                // emitted at INFO_GTE(NAME, 2) when a reference-directory match
                // means no transfer is needed. Rendered by the CLI from the
                // MetadataReused event (cli::frontend::progress::render) so
                // the line lands ahead of the totals; emitting it via
                // info_log! would route it through the post-summary
                // diagnostic drain and break upstream ordering.
                //
                // upstream: generator.c:1140 - a `--compare-dest` match itemizes
                // `.f` against the compare basis (`sxp->st`), so the attribute
                // columns reflect drift vs the basis, not the absent
                // destination. Compare source against the basis with
                // `destination_previously_existed = true` so identical files
                // stay blank.
                context.summary_mut().record_regular_file_matched();
                let basis_metadata = fs::symlink_metadata(&basis).map_err(|error| {
                    LocalCopyError::io("inspect compare-dest basis", basis.clone(), error)
                })?;
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                let total_bytes = Some(metadata_snapshot.len());
                let change_set = LocalCopyChangeSet::for_file(
                    metadata,
                    Some(&basis_metadata),
                    &metadata_options,
                    true,
                    false,
                    {
                        #[cfg(all(unix, feature = "xattr"))]
                        {
                            preserve_xattrs
                        }
                        #[cfg(not(all(unix, feature = "xattr")))]
                        {
                            false
                        }
                    },
                    {
                        #[cfg(all(any(unix, windows), feature = "acl"))]
                        {
                            preserve_acls
                        }
                        #[cfg(not(all(any(unix, windows), feature = "acl")))]
                        {
                            false
                        }
                    },
                );
                context.record(
                    LocalCopyRecord::new(
                        record_path.to_path_buf(),
                        LocalCopyAction::MetadataReused,
                        0,
                        total_bytes,
                        Duration::default(),
                        Some(metadata_snapshot),
                    )
                    .with_change_set(change_set),
                );
                context.register_progress();
                remove_source_entry_if_requested(context, source, Some(record_path), file_type)?;
                return Ok(LinkOutcome {
                    copy_source_override: None,
                    reference_basis: None,
                    completed: true,
                });
            }
            ReferenceDecision::Copy(path) => {
                // upstream: generator.c:1033-1039 - copy_altdest_file() copies
                // the basis into place and itemizes as ITEM_LOCAL_CHANGE (`c`).
                // The copy still flows through the transfer pipeline via
                // `copy_source_override`; `reference_basis` flags the record so
                // it is attributed to the basis instead of a network transfer.
                reference_basis = Some(path.clone());
                copy_source_override = Some(path);
            }
            ReferenceDecision::Link(path) => {
                if existing_metadata.is_some() {
                    remove_existing_destination(destination)?;
                }

                let link_result = create_hard_link(&path, destination);
                let mut degrade_to_copy = false;
                match link_result {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        remove_existing_destination(destination)?;
                        create_hard_link(&path, destination).map_err(|link_error| {
                            LocalCopyError::io(
                                "create hard link",
                                destination.to_path_buf(),
                                link_error,
                            )
                        })?;
                    }
                    Err(error)
                        if matches!(
                            error.raw_os_error(),
                            Some(code) if code == CROSS_DEVICE_ERROR_CODE
                        ) =>
                    {
                        degrade_to_copy = true;
                    }
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "create hard link",
                            destination.to_path_buf(),
                            error,
                        ));
                    }
                }

                if degrade_to_copy {
                    // upstream: generator.c:1031 try_a_copy - a cross-device
                    // hard-link failure falls back to copy_altdest_file(), which
                    // still itemizes as a local change (`c`).
                    reference_basis = Some(path.clone());
                    copy_source_override = Some(path);
                } else if copy_source_override.is_none() {
                    apply_file_metadata_with_options(destination, metadata, &metadata_options)
                        .map_err(map_metadata_error)?;
                    #[cfg(all(unix, feature = "xattr"))]
                    sync_xattrs_if_requested(
                        preserve_xattrs,
                        mode,
                        source,
                        destination,
                        true,
                        context.filter_program(),
                    )?;
                    // The destination is hardlinked to `path`, so all
                    // destinations sharing this reference share one inode.
                    // ACLs live on the inode (NTFS DACL on Windows, POSIX
                    // ACL on Unix), so the leader's write populates the
                    // shared inode and followers inherit for free.
                    //
                    // upstream: hlink.c::hard_link_check returns 1 for
                    // followers; generator.c:1540 exits before
                    // set_file_attrs() so set_acl() is never invoked on a
                    // follower alias.
                    #[cfg(all(any(unix, windows), feature = "acl"))]
                    if context.register_acl_cohort_leader(&path) {
                        sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
                    }
                    // upstream: hlink.c:236 - "%s => %s" at INFO_GTE(NAME, 1)
                    // when a hard link is created from a reference directory.
                    info_log!(Name, 1, "{} => {}", record_path.display(), path.display());
                    context.record_hard_link(metadata, destination);
                    context.summary_mut().record_hard_link();
                    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                    let total_bytes = Some(metadata_snapshot.len());
                    context.record(LocalCopyRecord::new(
                        record_path.to_path_buf(),
                        LocalCopyAction::HardLink,
                        0,
                        total_bytes,
                        Duration::default(),
                        Some(metadata_snapshot),
                    ));
                    context.register_created_path(
                        destination,
                        CreatedEntryKind::HardLink,
                        destination_previously_existed,
                    );
                    context.register_progress();
                    remove_source_entry_if_requested(
                        context,
                        source,
                        Some(record_path),
                        file_type,
                    )?;
                    return Ok(LinkOutcome {
                        copy_source_override: None,
                        reference_basis: None,
                        completed: true,
                    });
                }
            }
        }
    }

    Ok(LinkOutcome {
        copy_source_override,
        reference_basis,
        completed: false,
    })
}

#[cfg(test)]
mod tests {
    //! Pinning tests for `--info=NAME` level 1/2 emissions in the local-copy
    //! link path. Strings are matched byte-for-byte against upstream rsync.
    //!
    //! upstream: log.c log_item / send_directory NAME emissions
    //! upstream: hlink.c:236 ("=>"), generator.c:1010/1133 ("is uptodate").

    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, info_log, init};

    fn init_name_level(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.info.name = level;
        init(cfg);
        let _ = drain_events();
    }

    fn name_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Info {
                    flag: InfoFlag::Name,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn local_copy_hard_link_emission_matches_upstream() {
        // upstream: hlink.c:236 - "%s => %s" at INFO_GTE(NAME, 1)
        init_name_level(1);
        info_log!(Name, 1, "{} => {}", "out/file.txt", "ref/file.txt");
        let msgs = name_messages();
        assert!(
            msgs.iter().any(|m| m == "out/file.txt => ref/file.txt"),
            "missing upstream hardlink => wording: {msgs:?}"
        );
    }

    #[test]
    fn local_copy_uptodate_emission_matches_upstream() {
        // upstream: rsync.c:672-676 - "%s is uptodate" at INFO_GTE(NAME, 2)
        init_name_level(2);
        info_log!(Name, 2, "{} is uptodate", "out/file.txt");
        let msgs = name_messages();
        assert!(
            msgs.iter().any(|m| m == "out/file.txt is uptodate"),
            "missing upstream `is uptodate` wording: {msgs:?}"
        );
    }

    #[test]
    fn local_copy_uptodate_suppressed_below_level_two() {
        // upstream: rsync.c:672 - INFO_GTE(NAME, 2) gates the emission
        init_name_level(1);
        info_log!(Name, 2, "{} is uptodate", "out/file.txt");
        assert!(
            name_messages().is_empty(),
            "uptodate emission must be gated at NAME level 2"
        );
    }

    /// Windows-only regression test for POST-4 / WAS-6 (#2415, audit PR
    /// #4399). The `--copy-dest` Link branch hardlinks every cohort follower
    /// to the same reference inode. On NTFS the DACL lives on the MFT
    /// record, so each `SetNamedSecurityInfoW` call writes to the shared
    /// inode regardless of which alias was opened.
    ///
    /// Before the fix the loop invoked `sync_acls_if_requested` once per
    /// follower (O(N) per N-link cohort). The cohort gate routes the write
    /// through the first call only; followers inherit through the inode.
    ///
    /// upstream: hlink.c::hard_link_check returns 1 for followers so the
    /// generator never calls `set_file_attrs()` -> `set_acl()` on a
    /// follower alias.
    #[cfg(target_os = "windows")]
    #[test]
    fn copy_dest_link_branch_dacl_writes_once_per_cohort() {
        use std::path::Path;

        use crate::local_copy::HardLinkTracker;

        let mut tracker = HardLinkTracker::new();
        let reference = Path::new(r"C:\ref\leader.bin");
        let followers: [&Path; 5] = [
            Path::new(r"C:\dst\f1.bin"),
            Path::new(r"C:\dst\f2.bin"),
            Path::new(r"C:\dst\f3.bin"),
            Path::new(r"C:\dst\f4.bin"),
            Path::new(r"C:\dst\f5.bin"),
        ];

        let mut dacl_writes = 0_usize;
        for _follower in followers {
            // Mirrors the production gate in `process_links` at the
            // `ReferenceDecision::Link` branch.
            if tracker.register_acl_cohort_leader(reference) {
                dacl_writes += 1;
            }
        }

        assert_eq!(
            dacl_writes, 1,
            "5-link cohort must trigger exactly one DACL write (was: {dacl_writes})"
        );
    }
}
