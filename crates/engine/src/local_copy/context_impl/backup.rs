// `--backup` / `--backup-dir` staging of an existing destination entry,
// and the forced removal that makes room for a replacement.
// upstream: backup.c `make_backup()`, `keep_backup()`.
impl<'a> CopyContext<'a> {
    /// Reports whether `--safe-links` forbids backing up `destination`,
    /// emitting upstream's `SYMSAFE,1` notice when it does.
    ///
    /// upstream: `backup.c:289-310` gates the hard-link/rename fast path and
    /// `backup.c:368-375` gates the copy fallback on the same rule. Upstream
    /// carries both because `link_or_rename()` would otherwise hard-link an
    /// escaping symlink into the backup area and `goto success`, never reaching
    /// the copy path's check - its own comment at `backup.c:282-288` says so.
    /// Routing both oc sites through one predicate is what stops them drifting
    /// apart again, which is exactly how the fast path came to be unguarded.
    ///
    /// A `readlink` failure refuses the backup rather than permitting it:
    /// upstream fails closed at `backup.c:295-300` on the same reasoning, that
    /// an unverifiable target must not be preserved into the backup area.
    fn backup_refused_by_safe_links(&self, destination: &Path, file_type: fs::FileType) -> bool {
        if !(file_type.is_symlink()
            && self.options.links_enabled()
            && self.options.safe_links_enabled())
        {
            return false;
        }

        let Ok(target) = fs::read_link(destination) else {
            // upstream: backup.c:296-297
            info_log!(
                Symsafe,
                1,
                "not backing up symlink with unreadable target \"{}\"",
                destination.display()
            );
            return true;
        };

        let safety_rel = destination
            .strip_prefix(self.destination_root())
            .unwrap_or(destination);
        if symlink_target_is_safe(&target, safety_rel) {
            return false;
        }

        // upstream: backup.c:303-306
        info_log!(
            Symsafe,
            1,
            "not backing up unsafe symlink \"{}\" -> \"{}\"",
            destination.display(),
            target.display()
        );
        true
    }

    /// Hard-links, renames, or copies an existing destination entry to the
    /// backup location when `--backup` is enabled.
    ///
    /// `prefer_rename` mirrors upstream's `make_backup(fname, prefer_rename)`
    /// parameter: callers backing up an item that is about to be unlinked
    /// outright (the delete pass) pass `true` to skip straight to the rename
    /// tier, since the caller removes the original right after regardless of
    /// which strategy placed the backup (`delete.c:165-167`). Callers backing
    /// up an item before overwriting it with fresh content pass `false` so
    /// the hard-link tier runs first (`rsync.c:740`, `receiver.c:538`).
    ///
    /// Emits an `--info=BACKUP` notice mirroring upstream rsync 3.4.1
    /// (backup.c:352) under `INFO_GTE(BACKUP, 1)` once the backup has been
    /// placed successfully. The wording matches upstream byte-for-byte:
    /// `backed up <fname> to <buf>`.
    pub(super) fn backup_existing_entry(
        &mut self,
        destination: &Path,
        _relative: Option<&Path>,
        file_type: fs::FileType,
        prefer_rename: bool,
    ) -> Result<(), LocalCopyError> {
        if !self.options.backup_enabled() || self.mode.is_dry_run() {
            return Ok(());
        }

        if file_type.is_dir() {
            return Ok(());
        }

        // Always derive the relative path from destination/destination_root
        // rather than using the source-relative path. The source-relative path
        // may not include the source directory basename (e.g., "nested/file.txt"
        // instead of "source/nested/file.txt"), causing backup files to be
        // placed at the wrong location when --backup-dir is used.
        // upstream: backup.c:get_backup_name() uses the destination-relative path
        let backup_path = compute_backup_path(
            self.destination_root(),
            destination,
            None,
            self.options.backup_directory(),
            self.options.backup_suffix(),
        );

        if let Some(parent) = backup_path.parent()
            && !parent.as_os_str().is_empty()
        {
            // upstream: backup.c:copy_valid_path() - create any new backup
            // subdirectories, mirroring the source dir attrs onto each and
            // clearing non-directory obstructions.
            create_backup_parents(
                self.destination_root(),
                self.options.backup_directory(),
                parent,
                &self.metadata_options(),
            )?;
        }

        // upstream: backup.c:289-310 - honour --safe-links BEFORE the
        // hard-link/rename fast path. Without this the fast path preserves an
        // escaping symlink in the backup area and returns successfully, so the
        // cross-device check further down is never consulted.
        if self.backup_refused_by_safe_links(destination, file_type) {
            return Ok(());
        }

        // upstream: backup.c:200-207 link_or_rename() - try a hard link into
        // the backup area first when the caller doesn't prefer a rename
        // outright. A successful link leaves the destination's inode as the
        // backup, so no metadata reapply is needed below (same as RENAME).
        let hard_linked = if prefer_rename {
            None
        } else {
            match create_backup_hard_link(destination, &backup_path) {
                Ok(()) => Some(BackupStrategy::HardLink),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                // upstream: backup.c:247-256 - a stale backup entry is
                // removed and the hard link retried once.
                Err(error) if is_stale_backup_conflict(&error) => {
                    remove_stale_backup_entry(&backup_path)?;
                    match create_backup_hard_link(destination, &backup_path) {
                        Ok(()) => Some(BackupStrategy::HardLink),
                        Err(_) => None,
                    }
                }
                // Any other failure (e.g. EXDEV across a `--backup-dir` on a
                // different filesystem, or a type the platform cannot
                // hard-link) falls through to the rename tier below, mirroring
                // `link_or_rename`'s in-call rename fallback.
                Err(_) => None,
            }
        };

        // Track which backup strategy succeeded so we can emit the matching
        // upstream `--debug=BACKUP` trace (HLINK, RENAME, COPY, SYMLINK, or
        // DEVICE).
        // upstream: backup.c:link_or_rename and the fall-through copy_file path.
        let strategy = if let Some(strategy) = hard_linked {
            strategy
        } else {
            match backup_rename(destination, &backup_path) {
                Ok(()) => BackupStrategy::Rename,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                // upstream: backup.c:247-256 - link_or_rename failing with EEXIST or
                // EISDIR is recoverable: lstat the target and call delete_item with
                // DEL_RECURSE, then retry. EISDIR fires when the backup-dir already
                // contains a directory at the path we need (e.g. user pre-created
                // it, or a previous backup left a tree there); without this arm,
                // backup test 4 from upstream backup.test fails as exit-23 fatal.
                // Windows reports renaming a file onto an existing directory as
                // ERROR_ACCESS_DENIED (PermissionDenied) rather than EEXIST/EISDIR,
                // so it must enter the same recovery arm; the inner symlink_metadata
                // re-stat below still gates removal on the target actually existing,
                // so a genuine permission error falls through to the retry-and-fail
                // path unchanged.
                Err(error) if is_stale_backup_conflict(&error) => {
                    remove_stale_backup_entry(&backup_path)?;
                    fs::rename(destination, &backup_path).map_err(|rename_error| {
                        LocalCopyError::io("create backup", backup_path.clone(), rename_error)
                    })?;
                    BackupStrategy::Rename
                }
                Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                    // upstream: backup.c:368-375 - the copy fallback re-checks
                    // --safe-links before recreating the symlink. Upstream keeps
                    // this alongside the fast-path check above; both go through the
                    // one predicate so they cannot implement different rules.
                    if self.backup_refused_by_safe_links(destination, file_type) {
                        return Ok(());
                    }
                    match copy_entry_to_backup(
                        destination,
                        &backup_path,
                        file_type,
                        self.options.devices_enabled(),
                        self.options.specials_enabled(),
                        self.options.fake_super_enabled(),
                    )? {
                        Some(strategy) => strategy,
                        // upstream: backup.c:306-317 - a non-regular file that is
                        // neither backed up as a device/special (gates off) nor a
                        // symlink is skipped; make_backup returns 3 and leaves no
                        // backup, so emit no trace and no "backed up" notice.
                        None => return Ok(()),
                    }
                }
                Err(error) => {
                    return Err(LocalCopyError::io("create backup", backup_path, error));
                }
            }
        };

        // upstream: backup.c:338-341 - set_file_attrs(buf, file, NULL, fname,
        // ATTRS_ACCURATE_TIME) copies the source node's mode/owner/times onto
        // the freshly-created backup node (with preserve_xattrs temporarily
        // cleared), overriding the umask/copy defaults. Every cross-device copy
        // fallback needs this - regular file (COPY), symlink (SYMLINK), and
        // device/special (DEVICE) - because fs::copy/create_symlink/mknod leave
        // the backup owned by the caller (root), whereas upstream reapplies the
        // source uid/gid/mode/mtime. The same-fs rename path carries the inode's
        // attributes verbatim and is skipped here.
        if matches!(
            strategy,
            BackupStrategy::Copy | BackupStrategy::Symlink | BackupStrategy::Device
        ) && let Ok(source_meta) = fs::symlink_metadata(destination)
        {
            let metadata_options = self.metadata_options();
            // upstream: rsync.c:set_file_attrs() skips chmod for symlinks and
            // applies ownership/times with AT_SYMLINK_NOFOLLOW.
            if strategy == BackupStrategy::Symlink {
                apply_symlink_metadata_with_options(&backup_path, &source_meta, &metadata_options)
                    .map_err(map_metadata_error)?;
            } else {
                apply_file_metadata_with_options(&backup_path, &source_meta, &metadata_options)
                    .map_err(map_metadata_error)?;
            }
            // upstream: xattrs.c:set_stat_xattr() re-records the source stat in
            // `user.rsync.%stat` under --fake-super so the virtualised node's
            // mode/owner/rdev survive; no-op when fake-super is off.
            #[cfg(all(unix, feature = "xattr"))]
            store_effective_fake_super_if_requested(
                &metadata_options,
                destination,
                &backup_path,
                &source_meta,
            )?;
        }

        // upstream: backup.c:201-202,216-217,282-283,299-300,333-334 -
        // DEBUG_GTE(BACKUP, 1) emits one of HLINK/RENAME/DEVICE/SYMLINK/COPY
        // per success path. oc-rsync's local-copy executor prefers a
        // same-filesystem hard link, falls back to rename, then falls back to
        // copy or symlink recreation across filesystem boundaries.
        let destination_display = destination.display().to_string();
        match strategy {
            BackupStrategy::HardLink => trace_make_backup_hlink(&destination_display),
            BackupStrategy::Rename => trace_make_backup_rename(&destination_display),
            BackupStrategy::Copy => trace_make_backup_copy(&destination_display),
            BackupStrategy::Symlink => trace_make_backup_symlink(&destination_display),
            BackupStrategy::Device => trace_make_backup_device(&destination_display),
        }

        // upstream: backup.c:433 - rprintf(FINFO, "backed up %s to %s\n", fname, buf)
        // emits fname and buf as the rsync-relative paths (e.g. "deep/name1"),
        // not absolute filesystem paths. Strip the destination_root prefix so
        // the message matches upstream byte-for-byte and grep-by-relative-path
        // assertions in the upstream backup.test pass.
        let dest_root = self.destination_root();
        let destination_rel = destination.strip_prefix(dest_root).unwrap_or(destination);
        let backup_rel = backup_path
            .strip_prefix(dest_root)
            .unwrap_or(backup_path.as_path());
        info_log!(
            Backup,
            1,
            "backed up {} to {}",
            destination_rel.display(),
            backup_rel.display()
        );

        Ok(())
    }

    /// Backs up an existing regular destination file by COPYING it aside,
    /// leaving the destination inode in place for an `--inplace` rewrite.
    ///
    /// Unlike [`backup_existing_entry`](Self::backup_existing_entry), which
    /// renames the destination (moving its inode to the backup name), this
    /// duplicates the pre-transfer contents so the original inode stays put and
    /// is updated in place - the whole point of `--inplace`. Only used for
    /// regular-file inplace updates; other entries keep the rename path.
    ///
    /// upstream: backup.c make_backup() inplace copy path - the generator makes
    /// the backup a COPY (`generator.c:1862` `copy_file(fname, backupptr, ...)`,
    /// with the delta twin building the same copy at `generator.c:1898`) BEFORE
    /// the receiver rewrites the destination in place. The inplace copy bypasses
    /// `make_backup()`, so it emits no `DEBUG_GTE(BACKUP, 1)` trace, but it still
    /// emits the `INFO_GTE(BACKUP, 1)` "backed up X to Y" line
    /// (`generator.c:1990-1992`).
    pub(super) fn backup_existing_entry_copy(
        &mut self,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        let backup_path = compute_backup_path(
            self.destination_root(),
            destination,
            None,
            self.options.backup_directory(),
            self.options.backup_suffix(),
        );

        if let Some(parent) = backup_path.parent()
            && !parent.as_os_str().is_empty()
        {
            // upstream: backup.c:copy_valid_path() - create any new backup
            // subdirectories, mirroring the source dir attrs onto each and
            // clearing non-directory obstructions.
            create_backup_parents(
                self.destination_root(),
                self.options.backup_directory(),
                parent,
                &self.metadata_options(),
            )?;
        }

        // upstream: generator.c:1866 copy_file() - duplicate the pre-image; the
        // destination inode is left in place to be rewritten by the inplace
        // writer. A pre-existing backup is overwritten (upstream robust_unlinks
        // it at generator.c:1901); the O_TRUNC create reaches the same end
        // state. The backup path is operator-named, so it resolves through the
        // ownership walk - generator.c:2283 raises `operator_path_resolve` for
        // exactly this copy because it bypasses `make_backup()`.
        match copy_pre_image_to_backup(destination, &backup_path) {
            Ok(_) => {}
            // A vanished destination has nothing to back up, mirroring the
            // NotFound arm of the rename path (backup.c:make_backup returns 3).
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(LocalCopyError::io("create backup", backup_path, error));
            }
        }

        // upstream: generator.c:1988 set_file_attrs(backupptr, back_file, ...) -
        // the backup carries the source node's mode/owner/times. fs::copy
        // preserves only permissions, so reapply full metadata like the
        // cross-device COPY fallback in backup_existing_entry.
        if let Ok(source_meta) = fs::symlink_metadata(destination) {
            let metadata_options = self.metadata_options();
            apply_file_metadata_with_options(&backup_path, &source_meta, &metadata_options)
                .map_err(map_metadata_error)?;
            // upstream: generator.c:1985 copy_xattrs() under --fake-super
            // re-records the source stat in `user.rsync.%stat`; no-op otherwise.
            #[cfg(all(unix, feature = "xattr"))]
            store_effective_fake_super_if_requested(
                &metadata_options,
                destination,
                &backup_path,
                &source_meta,
            )?;
        }

        // upstream: generator.c:1990-1992 - INFO_GTE(BACKUP, 1) "backed up X to
        // Y". No DEBUG_GTE(BACKUP) trace: the inplace copy bypasses make_backup().
        let dest_root = self.destination_root();
        let destination_rel = destination.strip_prefix(dest_root).unwrap_or(destination);
        let backup_rel = backup_path
            .strip_prefix(dest_root)
            .unwrap_or(backup_path.as_path());
        info_log!(
            Backup,
            1,
            "backed up {} to {}",
            destination_rel.display(),
            backup_rel.display()
        );

        Ok(())
    }

    /// Forcibly removes a type-conflicting destination entry (backing it up
    /// first if needed) to make room for an incoming item of a different type.
    ///
    /// upstream: generator.c:1240 recv_generator() clears the conflicting
    /// destination with `delete_item(fname, mode, del_opts | DEL_FOR_FILE)`.
    /// delete.c:178-194 `delete_item()` suppresses `log_delete()` and the
    /// `stats.deleted_files` bump whenever `flags & DEL_MAKE_ROOM` is set, so
    /// the make-room removal of the conflicting entry itself is silent and
    /// uncounted (unlike a genuine delete-pass deletion). When that entry is a
    /// directory, `delete_dir_contents()` (delete.c:83) recurses with
    /// DEL_MAKE_ROOM stripped, so its contents are itemized (`*deleting`) and
    /// counted like ordinary deletions while the directory node stays silent.
    pub(super) fn force_remove_destination(
        &mut self,
        destination: &Path,
        relative: Option<&Path>,
        metadata: &fs::Metadata,
    ) -> Result<(), LocalCopyError> {
        let file_type = metadata.file_type();

        if self.mode.is_dry_run() {
            if file_type.is_dir() {
                self.record_make_room_contents(destination, relative)?;
            }
            self.register_progress();
            return Ok(());
        }

        // upstream: delete.c:165-167 - `delete_item()` (the callee behind
        // generator.c:1240's DEL_FOR_FILE removal) always calls
        // `make_backup(fbuf, True)`, so the hard-link tier is skipped here
        // exactly as it is for a genuine delete-pass removal.
        self.backup_existing_entry(destination, relative, file_type, true)?;

        if file_type.is_dir() {
            self.record_make_room_contents(destination, relative)?;
        }

        let context = if file_type.is_dir() {
            "remove existing directory"
        } else {
            "remove existing destination"
        };

        let removal_result = if file_type.is_dir() {
            fs::remove_dir_all(destination)
        } else {
            fs::remove_file(destination)
        };

        match removal_result {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    context,
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        self.register_progress();

        Ok(())
    }

    /// Itemizes and counts the contents of a conflicting directory that is
    /// being cleared to make room for an incoming item, mirroring upstream's
    /// `delete_dir_contents()` recursion (delete.c:83): the children are
    /// reported like delete-pass deletions (DEL_MAKE_ROOM stripped) while the
    /// directory node itself is removed silently by the caller.
    fn record_make_room_contents(
        &mut self,
        destination: &Path,
        relative: Option<&Path>,
    ) -> Result<(), LocalCopyError> {
        let mut subtree_path = destination.to_path_buf();
        let mut subtree_relative = relative.map(Path::to_path_buf).unwrap_or_else(|| {
            destination
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_default()
        });
        record_directory_subtree(self, &mut subtree_path, &mut subtree_relative)
    }
}

/// Returns `true` when a hard-link or rename attempt into the backup area
/// failed because a stale entry already occupies `backup_path`.
///
/// upstream: `backup.c:247` - `link_or_rename()` fails with `EEXIST` or
/// `EISDIR` when the backup path is already occupied. Windows reports
/// renaming or linking onto an existing directory as `ERROR_ACCESS_DENIED`
/// (`PermissionDenied`) rather than `EEXIST`/`EISDIR`, so it is treated the
/// same way; [`remove_stale_backup_entry`]'s re-stat still gates removal on
/// the target actually existing, so a genuine permission error falls through
/// to the retry-and-fail path unchanged.
fn is_stale_backup_conflict(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::AlreadyExists
            | io::ErrorKind::IsADirectory
            | io::ErrorKind::PermissionDenied
    )
}

/// Clears whatever occupies `backup_path` so a hard-link or rename retry can
/// land cleanly.
///
/// upstream: `backup.c:247-256` - `make_backup()` lstats the stale backup
/// target and calls `delete_item(...DEL_FOR_BACKUP | DEL_RECURSE)` before
/// retrying `link_or_rename()`.
fn remove_stale_backup_entry(backup_path: &Path) -> Result<(), LocalCopyError> {
    match fs::symlink_metadata(backup_path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(backup_path).map_err(|remove_error| {
            LocalCopyError::io(
                "remove existing backup directory",
                backup_path,
                remove_error,
            )
        }),
        Ok(_) => {
            if let Err(remove_error) = fs::remove_file(backup_path)
                && remove_error.kind() != io::ErrorKind::NotFound
            {
                return Err(LocalCopyError::io(
                    "remove existing backup",
                    backup_path,
                    remove_error,
                ));
            }
            Ok(())
        }
        Err(meta_error) if meta_error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(meta_error) => Err(LocalCopyError::io(
            "stat existing backup",
            backup_path,
            meta_error,
        )),
    }
}
