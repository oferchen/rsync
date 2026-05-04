use std::num::NonZeroU32;

impl<'a> CopyContext<'a> {
    /// Builds a [`MetadataOptions`] snapshot from the current copy options.
    pub(super) fn metadata_options(&self) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_owner(self.options.preserve_owner())
            .preserve_group(self.options.preserve_group())
            .preserve_executability(self.options.preserve_executability())
            .preserve_permissions(self.options.preserve_permissions())
            .preserve_times(self.options.preserve_times())
            .preserve_atimes(self.options.preserve_atimes())
            .preserve_crtimes(self.options.preserve_crtimes())
            .numeric_ids(self.options.numeric_ids_enabled())
            .fake_super(self.options.fake_super_enabled())
            .with_owner_override(self.options.owner_override())
            .with_group_override(self.options.group_override())
            .with_chmod(self.options.chmod().cloned())
            .with_user_mapping(self.options.user_mapping().cloned())
            .with_group_mapping(self.options.group_mapping().cloned())
    }

    pub(super) const fn copy_links_enabled(&self) -> bool {
        self.options.copy_links_enabled()
    }

    pub(super) const fn links_enabled(&self) -> bool {
        self.options.links_enabled()
    }

    pub(super) const fn copy_unsafe_links_enabled(&self) -> bool {
        self.options.copy_unsafe_links_enabled()
    }

    pub(super) const fn safe_links_enabled(&self) -> bool {
        self.options.safe_links_enabled()
    }

    pub(super) const fn munge_links_enabled(&self) -> bool {
        self.options.munge_links_enabled()
    }

    /// Sets the safety depth offset.  Call with `1` before entering a
    /// whole-directory copy (no trailing slash), `0` for a contents copy.
    pub(super) fn set_safety_depth_offset(&mut self, offset: usize) {
        self.safety_depth_offset = offset;
    }

    /// Strips the transfer-root prefix from a relative path, producing a
    /// path suitable for `symlink_target_is_safe`.
    pub(super) fn strip_safety_prefix<'p>(&self, relative: &'p Path) -> &'p Path {
        if self.safety_depth_offset == 0 {
            return relative;
        }
        let mut components = relative.components();
        for _ in 0..self.safety_depth_offset {
            components.next();
        }
        components.as_path()
    }

    pub(super) const fn copy_dirlinks_enabled(&self) -> bool {
        self.options.copy_dirlinks_enabled()
    }

    pub(super) const fn keep_dirlinks_enabled(&self) -> bool {
        self.options.keep_dirlinks_enabled()
    }

    pub(super) const fn whole_file_enabled(&self) -> bool {
        self.options.whole_file_enabled()
    }

    pub(super) const fn open_noatime_enabled(&self) -> bool {
        self.options.open_noatime_enabled()
    }

    pub(super) const fn sparse_enabled(&self) -> bool {
        self.options.sparse_enabled()
    }

    pub(super) const fn append_enabled(&self) -> bool {
        self.options.append_enabled()
    }

    pub(super) const fn append_verify_enabled(&self) -> bool {
        self.options.append_verify_enabled()
    }

    pub(super) const fn preallocate_enabled(&self) -> bool {
        self.options.preallocate_enabled()
    }

    #[allow(dead_code)] // Accessor retained for future use; DeferredSync handles runtime selection
    pub(super) const fn fsync_enabled(&self) -> bool {
        self.options.fsync_enabled()
    }

    pub(super) const fn devices_enabled(&self) -> bool {
        self.options.devices_enabled()
    }

    pub(super) const fn copy_devices_as_files_enabled(&self) -> bool {
        self.options.copy_devices_as_files_enabled()
    }

    pub(super) const fn specials_enabled(&self) -> bool {
        self.options.specials_enabled()
    }

    pub(super) const fn force_replacements_enabled(&self) -> bool {
        self.options.force_replacements_enabled()
    }

    /// Returns the filter program used by xattr sync logic.
    #[cfg(all(unix, feature = "xattr"))]
    pub(super) const fn filter_program(
        &self,
    ) -> Option<&crate::local_copy::filter_program::FilterProgram> {
        self.filter_program.as_ref()
    }

    /// Reports whether ACL preservation is enabled.
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub(super) const fn acls_enabled(&self) -> bool {
        self.options.acls_enabled()
    }

    pub(super) const fn relative_paths_enabled(&self) -> bool {
        self.options.relative_paths_enabled()
    }

    pub(super) const fn recursive_enabled(&self) -> bool {
        self.options.recursive_enabled()
    }

    pub(super) const fn dirs_enabled(&self) -> bool {
        self.options.dirs_enabled()
    }

    pub(super) const fn implied_dirs_enabled(&self) -> bool {
        self.options.implied_dirs_enabled()
    }

    pub(super) const fn mkpath_enabled(&self) -> bool {
        self.options.mkpath_enabled()
    }

    pub(super) const fn prune_empty_dirs_enabled(&self) -> bool {
        self.options.prune_empty_dirs_enabled()
    }

    pub(super) const fn omit_dir_times_enabled(&self) -> bool {
        self.options.omit_dir_times_enabled()
    }

    pub(super) const fn omit_link_times_enabled(&self) -> bool {
        self.options.omit_link_times_enabled()
    }

    fn parent_relative_to_destination<'p>(&self, parent: &'p Path) -> Option<&'p Path> {
        parent
            .strip_prefix(self.destination_root())
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
    }

    /// Dry-run replacement policy: remove destination (logically) and either
    /// allow creation (Ok) or synthesize a "NotFound" error.
    fn replace_parent_entry_dry_run(
        &mut self,
        parent: &Path,
        existing: &fs::Metadata,
        allow_creation: bool,
    ) -> Result<(), LocalCopyError> {
        if self.force_replacements_enabled() {
            let relative = self.parent_relative_to_destination(parent);
            self.force_remove_destination(parent, relative, existing)?;
            if allow_creation {
                Ok(())
            } else {
                Err(LocalCopyError::io(
                    "create parent directory",
                    parent.to_path_buf(),
                    io::Error::from(io::ErrorKind::NotFound),
                ))
            }
        } else {
            Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
            ))
        }
    }

    /// Creation policy when creation is allowed and side effects are real
    /// (non-dry-run): replace destination, create directory, and register progress.
    fn replace_parent_entry_create(
        &mut self,
        parent: &Path,
        existing: &fs::Metadata,
    ) -> Result<(), LocalCopyError> {
        if self.force_replacements_enabled() {
            let relative = self.parent_relative_to_destination(parent);
            self.force_remove_destination(parent, relative, existing)?;
            fs::create_dir_all(parent).map_err(|error| {
                LocalCopyError::io("create parent directory", parent, error)
            })?;
            self.register_progress();
            Ok(())
        } else {
            Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
            ))
        }
    }

    /// Policy when creation is forbidden: replace destination and always return
    /// the synthesized "NotFound" IO error (mirroring upstream behavior).
    fn replace_parent_entry_forbidden(
        &mut self,
        parent: &Path,
        existing: &fs::Metadata,
    ) -> Result<(), LocalCopyError> {
        if self.force_replacements_enabled() {
            let relative = self.parent_relative_to_destination(parent);
            self.force_remove_destination(parent, relative, existing)?;
            Err(LocalCopyError::io(
                "create parent directory",
                parent.to_path_buf(),
                io::Error::from(io::ErrorKind::NotFound),
            ))
        } else {
            Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
            ))
        }
    }

    /// Ensures the parent directory exists, creating it if `--implied-dirs` or
    /// `--mkpath` is enabled, or replacing a non-directory obstacle when
    /// `--force` is set.
    pub(super) fn prepare_parent_directory(&mut self, parent: &Path) -> Result<(), LocalCopyError> {
        if parent.as_os_str().is_empty() {
            return Ok(());
        }

        // Fast path: skip stat if this parent was already verified as a directory.
        // With 10K files in one directory, this avoids 9,999 redundant statx calls.
        if self.verified_parents.contains(parent) {
            return Ok(());
        }

        let allow_creation = self.implied_dirs_enabled() || self.mkpath_enabled();
        let keep_dirlinks = self.keep_dirlinks_enabled();

        let result = if self.mode.is_dry_run() {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        follow_symlink_metadata(parent).and_then(|metadata| {
                            if metadata.file_type().is_dir() {
                                Ok(())
                            } else {
                                self.replace_parent_entry_dry_run(parent, &existing, allow_creation)
                            }
                        })
                    } else {
                        self.replace_parent_entry_dry_run(parent, &existing, allow_creation)
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if allow_creation {
                        Ok(())
                    } else {
                        Err(LocalCopyError::io(
                            "create parent directory",
                            parent.to_path_buf(),
                            error,
                        ))
                    }
                }
                Err(error) => Err(LocalCopyError::io(
                    "inspect existing destination",
                    parent.to_path_buf(),
                    error,
                )),
            }
        } else if allow_creation {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        let metadata = follow_symlink_metadata(parent)?;
                        if metadata.file_type().is_dir() {
                            Ok(())
                        } else {
                            self.replace_parent_entry_create(parent, &existing)
                        }
                    } else {
                        self.replace_parent_entry_create(parent, &existing)
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir_all(parent).map_err(|error| {
                        LocalCopyError::io("create parent directory", parent, error)
                    })?;
                    self.register_progress();
                    Ok(())
                }
                Err(error) => Err(LocalCopyError::io(
                    "create parent directory",
                    parent.to_path_buf(),
                    error,
                )),
            }
        } else {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        let metadata = follow_symlink_metadata(parent)?;
                        if metadata.file_type().is_dir() {
                            Ok(())
                        } else {
                            self.replace_parent_entry_forbidden(parent, &existing)
                        }
                    } else {
                        self.replace_parent_entry_forbidden(parent, &existing)
                    }
                }
                Err(error) => Err(LocalCopyError::io(
                    "create parent directory",
                    parent.to_path_buf(),
                    error,
                )),
            }
        };

        if result.is_ok() {
            self.verified_parents.insert(parent.to_path_buf());
        }
        result
    }

    pub(super) const fn remove_source_files_enabled(&self) -> bool {
        self.options.remove_source_files_enabled()
    }

    pub(super) const fn compress_enabled(&self) -> bool {
        self.options.compress_enabled()
    }

    /// Returns whether compression should be used for this file, considering
    /// the skip-compress suffix list.
    pub(super) fn should_compress(&self, relative: &Path) -> bool {
        self.compress_enabled() && !self.options.should_skip_compress(relative)
    }

    pub(super) const fn compression_level(&self) -> CompressionLevel {
        self.options.compression_level()
    }

    pub(super) const fn compression_algorithm(&self) -> CompressionAlgorithm {
        self.options.compression_algorithm()
    }

    pub(super) const fn block_size_override(&self) -> Option<NonZeroU32> {
        self.options.block_size_override()
    }

    pub(super) const fn checksum_enabled(&self) -> bool {
        self.options.checksum_enabled()
    }

    pub(super) const fn size_only_enabled(&self) -> bool {
        self.options.size_only_enabled()
    }

    pub(super) const fn ignore_times_enabled(&self) -> bool {
        self.options.ignore_times_enabled()
    }

    pub(super) const fn ignore_existing_enabled(&self) -> bool {
        self.options.ignore_existing_enabled()
    }

    pub(super) const fn existing_only_enabled(&self) -> bool {
        self.options.existing_only_enabled()
    }

    pub(super) const fn ignore_missing_args_enabled(&self) -> bool {
        self.options.ignore_missing_args_enabled()
    }

    pub(super) const fn delete_missing_args_enabled(&self) -> bool {
        self.options.delete_missing_args_enabled()
    }

    pub(super) const fn update_enabled(&self) -> bool {
        self.options.update_enabled()
    }

    pub(super) const fn partial_enabled(&self) -> bool {
        self.options.partial_enabled()
    }

    pub(super) fn partial_directory_path(&self) -> Option<&Path> {
        self.options.partial_directory_path()
    }

    pub(super) fn temp_directory_path(&self) -> Option<&Path> {
        self.options.temp_directory_path()
    }

    pub(super) const fn inplace_enabled(&self) -> bool {
        self.options.inplace_enabled()
    }

    #[cfg(unix)]
    #[cfg(all(unix, feature = "xattr"))]
    pub(super) const fn xattrs_enabled(&self) -> bool {
        self.options.preserve_xattrs()
    }

    /// Evaluates filter rules to determine whether the entry is allowed for
    /// transfer. Returns `true` if the entry passes all filters.
    pub(super) fn allows(&self, relative: &Path, is_dir: bool) -> bool {
        if let Some(program) = &self.filter_program {
            let layers = self.dir_merge_layers.borrow();
            let ephemeral = self.dir_merge_ephemeral.borrow();
            let temp_layers = ephemeral.last().map(|entries| entries.as_slice());
            program
                .evaluate(
                    relative,
                    is_dir,
                    layers.as_slice(),
                    temp_layers,
                    FilterContext::Transfer,
                )
                .allows_transfer()
        } else if let Some(filters) = self.options.filter_set() {
            filters.allows(relative, is_dir)
        } else {
            true
        }
    }

    /// Returns `true` when a directory path is excluded by a non-directory-specific
    /// filter rule.
    ///
    /// Used by the planner when `--prune-empty-dirs` is active: directories
    /// excluded by generic patterns (e.g., `*`) should still be descended into
    /// so that file-level include rules can be evaluated. Only directory-specific
    /// exclude patterns (trailing `/`) should prevent traversal outright.
    pub(super) fn excluded_dir_by_non_dir_rule(&self, relative: &Path) -> bool {
        if let Some(program) = &self.filter_program {
            let layers = self.dir_merge_layers.borrow();
            let ephemeral = self.dir_merge_ephemeral.borrow();
            let temp_layers = ephemeral.last().map(|entries| entries.as_slice());
            program.excluded_dir_by_non_dir_rule(relative, layers.as_slice(), temp_layers)
        } else if let Some(filters) = self.options.filter_set() {
            filters.excluded_dir_by_non_dir_rule(relative)
        } else {
            false
        }
    }

    /// Evaluates filter rules to determine whether a destination entry may be
    /// deleted. Respects `--delete-excluded` when enabled.
    pub(super) fn allows_deletion(&self, relative: &Path, is_dir: bool) -> bool {
        let delete_excluded = self.options.delete_excluded_enabled();
        if let Some(program) = &self.filter_program {
            let layers = self.dir_merge_layers.borrow();
            let ephemeral = self.dir_merge_ephemeral.borrow();
            let temp_layers = ephemeral.last().map(|entries| entries.as_slice());
            let outcome = program.evaluate(
                relative,
                is_dir,
                layers.as_slice(),
                temp_layers,
                FilterContext::Deletion,
            );
            if delete_excluded {
                outcome.allows_deletion() || outcome.allows_deletion_when_excluded_removed()
            } else {
                outcome.allows_deletion()
            }
        } else if let Some(filters) = self.options.filter_set() {
            if delete_excluded {
                filters.allows_deletion(relative, is_dir)
                    || filters.allows_deletion_when_excluded_removed(relative, is_dir)
            } else {
                filters.allows_deletion(relative, is_dir)
            }
        } else {
            true
        }
    }

    /// Access the batch writer for recording transfer operations.
    ///
    /// Returns a reference to the batch writer if batch mode is enabled,
    /// or None if batch mode is not active.
    pub(super) const fn batch_writer(
        &self,
    ) -> Option<&std::sync::Arc<std::sync::Mutex<crate::batch::BatchWriter>>> {
        self.options.get_batch_writer()
    }

    /// Access the protocol flist writer for batch mode encoding.
    ///
    /// Returns a mutable reference to the [`FileListWriter`] used to encode
    /// file entries in the protocol wire format for batch files. The writer
    /// maintains cross-entry compression state.
    pub(super) fn batch_flist_writer_mut(
        &mut self,
    ) -> Option<&mut protocol::flist::FileListWriter> {
        self.batch_flist_writer.as_mut()
    }

    /// Writes the flist end-of-list marker to the batch file.
    ///
    /// Upstream rsync batch files are a raw tee of the protocol stream, which
    /// includes the end-of-list marker (0x00 byte for non-varint, varint(0) +
    /// varint(io_error) for varint mode) after all file entries. Without this
    /// marker, [`BatchReader::read_protocol_flist`] cannot determine where
    /// the file list ends and delta operations begin.
    ///
    /// Must be called after all file entries have been captured and before
    /// any delta operations or trailing stats are written.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_file_list()` writes the end-of-list marker after all
    ///   entries via `write_byte(f, 0)` (pre-varint) or the varint equivalent.
    pub(crate) fn finalize_batch_flist(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        let flist_writer = match self.batch_flist_writer.as_ref() {
            Some(w) => w,
            None => return Ok(()),
        };

        let mut buf = Vec::with_capacity(4);
        flist_writer.write_end(&mut buf, None).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch flist end marker",
                std::path::PathBuf::new(),
                e,
            )
        })?;

        let batch_writer_arc = match self.options.get_batch_writer() {
            Some(w) => w.clone(),
            None => return Ok(()),
        };
        let mut writer_guard = batch_writer_arc.lock().unwrap();
        writer_guard.write_data(&buf).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch flist end marker",
                std::path::PathBuf::new(),
                std::io::Error::other(e),
            )
        })?;

        Ok(())
    }

    /// Writes empty uid/gid ID lists to the batch file.
    ///
    /// upstream: uidlist.c:send_id_lists() - without INC_RECURSE, ID lists
    /// are written between the flist end marker and the delta data. Since
    /// user/group names are already embedded inline via XMIT_USER_NAME_FOLLOWS,
    /// the post-flist ID lists are empty (just varint30(0) terminators).
    ///
    /// Must be called after `finalize_batch_flist()` and before
    /// `flush_batch_delta_to_batch()`.
    pub(crate) fn write_batch_id_lists(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        let batch_writer_arc = match self.options.get_batch_writer() {
            Some(w) => w.clone(),
            None => return Ok(()),
        };

        let proto = batch_writer_arc.lock().unwrap().config().protocol_version;

        // upstream: uidlist.c:recv_id_list() reads uid list then gid list.
        // Each list: loop reading varint30 until 0 (no entries), then done.
        // Without xmit_id0_names (ID0_NAMES not in compat_flags), the
        // terminator is just varint30(0) for each list.
        let mut buf = Vec::with_capacity(2);
        protocol::write_varint30_int(&mut buf, 0, proto as u8).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch uid list terminator",
                std::path::PathBuf::new(),
                e,
            )
        })?;
        protocol::write_varint30_int(&mut buf, 0, proto as u8).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch gid list terminator",
                std::path::PathBuf::new(),
                e,
            )
        })?;

        let mut writer_guard = batch_writer_arc.lock().unwrap();
        writer_guard.write_data(&buf).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch id lists",
                std::path::PathBuf::new(),
                std::io::Error::other(e),
            )
        })?;

        Ok(())
    }

    /// Writes the iflags + sum_head preamble for a file's delta data
    /// to the per-file batch delta buffer.
    ///
    /// The NDX is NOT written here - it is deferred to flush time so that
    /// the correct sorted-order index can be used. upstream sorts the flist
    /// after reading it from the batch file, so NDX values must reference
    /// sorted positions, not traversal order.
    ///
    /// Must be called before any token writes for this file (before
    /// `capture_batch_whole_file` or inline delta token writes).
    ///
    /// upstream: sender.c:send_files() writes write_ndx_and_attrs() then
    /// write_sum_head() before delta tokens for each file.
    pub(crate) fn begin_batch_file_delta(
        &mut self,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        use std::io::Write;

        let delta_file = match self.batch_delta_buf.as_mut() {
            Some(f) => f,
            None => return Ok(()),
        };

        delta_file.get_mut().clear();
        delta_file.set_position(0);

        // NDX is remapped to sorted order at flush time; record the
        // traversal index here.
        self.batch_current_delta_idx = self.batch_flist_index - 1;

        // upstream: rsync.c:383 - write iflags (u16 LE) for protocol >= 29.
        // ITEM_TRANSFER (0x8000) indicates delta data follows.
        let batch_writer_arc = self.options.get_batch_writer().unwrap().clone();
        let proto = batch_writer_arc.lock().unwrap().config().protocol_version;
        if proto >= 29 {
            const ITEM_TRANSFER: u16 = 0x8000;
            delta_file
                .write_all(&ITEM_TRANSFER.to_le_bytes())
                .map_err(|e| {
                    crate::local_copy::LocalCopyError::io(
                        "write batch iflags",
                        std::path::PathBuf::new(),
                        e,
                    )
                })?;
        }

        // upstream: io.c:read_sum_head() / sender.c - write sum_head (4 x i32 LE).
        // For local copy whole-file transfers: count=0, blength=0, s2length=16
        // (MD5 checksum length), remainder=0.
        const FILE_SUM_LENGTH: i32 = 16;
        let count: i32 = 0;
        let blength: i32 = 0;
        let s2length: i32 = FILE_SUM_LENGTH;
        let remainder: i32 = 0;

        let mut sum_buf = [0u8; 16];
        sum_buf[0..4].copy_from_slice(&count.to_le_bytes());
        sum_buf[4..8].copy_from_slice(&blength.to_le_bytes());
        sum_buf[8..12].copy_from_slice(&s2length.to_le_bytes());
        sum_buf[12..16].copy_from_slice(&remainder.to_le_bytes());
        delta_file.write_all(&sum_buf).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch sum_head",
                std::path::PathBuf::new(),
                e,
            )
        })?;

        Ok(())
    }

    /// Writes a token-format end marker and file checksum to the batch delta
    /// buffer for the current file, then moves the completed per-file data
    /// to `batch_delta_entries`.
    ///
    /// Each file's delta data is terminated by write_int(0), matching upstream
    /// `token.c:simple_send_token()` with token=-1. After the token end, a
    /// file-level MD5 checksum of `s2length` bytes (16) is written, computed
    /// over the source file contents.
    ///
    /// upstream: match.c:370 sum_init(xfer_sum_nni, checksum_seed) then
    /// sum_update on file content then sum_end(sender_file_sum). For MD5
    /// (protocol >= 30), sum_init ignores the seed - the checksum is plain
    /// MD5 of the file bytes.
    ///
    /// upstream: receiver.c:408 - read_buf(f_in, sender_file_sum, xfer_sum_len)
    pub(crate) fn finalize_batch_file_delta(
        &mut self,
        source: &std::path::Path,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        use std::io::{Read, Write};

        let delta_file = match self.batch_delta_buf.as_mut() {
            Some(f) => f,
            None => return Ok(()),
        };

        // upstream: token.c - end-of-file marker is write_int(0)
        let mut buf = Vec::with_capacity(4);
        protocol::wire::delta::write_token_end(&mut buf).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch token end marker",
                std::path::PathBuf::new(),
                e,
            )
        })?;
        delta_file.write_all(&buf).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch token end marker",
                std::path::PathBuf::new(),
                e,
            )
        })?;

        // upstream: match.c:370-411 - compute MD5 of source file content.
        // For MD5 (protocol >= 30), sum_init() ignores checksum_seed.
        let file_sum = {
            let mut reader = std::fs::File::open(source).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "open source for batch checksum",
                    source.to_path_buf(),
                    e,
                )
            })?;
            let mut hasher = checksums::strong::Md5::new();
            let mut chunk = [0u8; 32 * 1024];
            loop {
                let n = reader.read(&mut chunk).map_err(|e| {
                    crate::local_copy::LocalCopyError::io(
                        "read source for batch checksum",
                        source.to_path_buf(),
                        e,
                    )
                })?;
                if n == 0 {
                    break;
                }
                hasher.update(&chunk[..n]);
            }
            hasher.finalize()
        };
        delta_file.write_all(&file_sum).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch file checksum",
                std::path::PathBuf::new(),
                e,
            )
        })?;

        // Move the completed per-file data to batch_delta_entries.
        // The NDX will be written at flush time using the sort-order mapping.
        let data = std::mem::take(delta_file.get_mut());
        delta_file.set_position(0);
        let idx = self.batch_current_delta_idx;
        self.batch_delta_entries.push((idx, data));

        Ok(())
    }

    /// Captures whole-file content to the batch delta buffer as token-format
    /// literals.
    ///
    /// When batch mode is active and the transfer does not use delta encoding
    /// (new file, whole-file mode, or no basis), the entire file content must
    /// still be captured so that replay can reconstruct it.
    ///
    /// upstream: match.c:match_sums() writes literals for whole-file transfers.
    pub(crate) fn capture_batch_whole_file(
        &mut self,
        source: &std::path::Path,
        file_size: u64,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        use std::io::Write;

        let delta_file = match self.batch_delta_buf.as_mut() {
            Some(_) => self.batch_delta_buf.as_mut().unwrap(),
            None => return Ok(()),
        };

        let mut reader = std::fs::File::open(source).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "open source for batch capture",
                source.to_path_buf(),
                e,
            )
        })?;

        let mut buf = vec![0u8; 32 * 1024]; // CHUNK_SIZE
        let mut remaining = file_size;

        while remaining > 0 {
            let to_read = (remaining as usize).min(buf.len());
            use std::io::Read;
            let n = reader.read(&mut buf[..to_read]).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "read source for batch capture",
                    source.to_path_buf(),
                    e,
                )
            })?;
            if n == 0 {
                break;
            }
            remaining = remaining.saturating_sub(n as u64);

            let mut encoded = Vec::with_capacity(n + 4);
            protocol::wire::delta::write_token_literal(&mut encoded, &buf[..n]).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "encode batch literal token",
                    source.to_path_buf(),
                    e,
                )
            })?;

            delta_file.write_all(&encoded).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch literal token",
                    source.to_path_buf(),
                    e,
                )
            })?;
        }

        Ok(())
    }

    /// Flushes all per-file delta entries to the batch writer with
    /// sort-order-corrected NDX values, then writes NDX_DONE phase markers.
    ///
    /// upstream sorts the flist after reading it from the batch file
    /// (`flist_sort_and_clean()`), so NDX values in the delta stream must
    /// reference sorted positions, not traversal order. This method builds
    /// the traversal-to-sorted mapping from `batch_entry_sort_data` and
    /// writes each file's NDX using its sorted position.
    ///
    /// Must be called after `finalize_batch_flist()` to produce the correct
    /// upstream batch ordering: all flist entries first, then all file data.
    ///
    /// upstream: sender.c:send_files() writes NDX_DONE after all files in
    /// phase 1, then again after phase 2 redo (protocol >= 29).
    pub(crate) fn flush_batch_delta_to_batch(
        &mut self,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        if self.batch_delta_buf.is_none() {
            return Ok(());
        }

        let batch_writer_arc = match self.options.get_batch_writer() {
            Some(w) => w.clone(),
            None => return Ok(()),
        };

        // Build traversal-index to sorted-index mapping.
        // upstream: flist.c:flist_sort_and_clean() sorts after recv_file_list().
        // We replicate the same sort on our entry names to determine where each
        // traversal-order entry ends up in the sorted flist.
        let traversal_to_sorted = self.build_batch_sort_mapping();

        // Write each file's delta data with the correct sorted NDX.
        let codec = self
            .batch_ndx_codec
            .as_mut()
            .expect("batch_ndx_codec must exist when batch_delta_buf is set");

        let mut entries = std::mem::take(&mut self.batch_delta_entries);
        // Sort entries by their post-sort NDX so the delta stream is in
        // ascending NDX order, matching what upstream's recv_files() expects.
        entries.sort_by_key(|(traversal_idx, _)| {
            traversal_to_sorted
                .get(*traversal_idx as usize)
                .copied()
                .unwrap_or(*traversal_idx)
        });
        for (traversal_idx, data) in &entries {
            let sorted_idx = traversal_to_sorted
                .get(*traversal_idx as usize)
                .copied()
                .unwrap_or(*traversal_idx);

            let mut ndx_buf = Vec::with_capacity(4);
            protocol::codec::NdxCodec::write_ndx(codec, &mut ndx_buf, sorted_idx).map_err(
                |e| {
                    crate::local_copy::LocalCopyError::io(
                        "write batch NDX",
                        std::path::PathBuf::new(),
                        e,
                    )
                },
            )?;
            let mut writer_guard = batch_writer_arc.lock().unwrap();
            writer_guard.write_data(&ndx_buf).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch NDX",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })?;
            writer_guard.write_data(data).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch delta data",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })?;
        }

        // Write NDX_DONE markers for phase transitions.
        //
        // upstream: receiver.c:recv_files() reads NDX_DONEs to transition
        // phases. With INC_RECURSE (protocol >= 30), the first NDX_DONE
        // frees the flist and falls through to phase increment. For
        // protocol >= 29, max_phase=2, so recv_files needs 3 NDX_DONEs
        // to break (phase 0->1->2->3, breaks when phase > max_phase).
        // For protocol < 29, max_phase=1, needs 2 NDX_DONEs.
        let proto = batch_writer_arc.lock().unwrap().config().protocol_version;
        let ndx_done_count = if proto >= 29 { 3 } else { 2 };

        for _ in 0..ndx_done_count {
            let mut done_buf = Vec::with_capacity(4);
            protocol::codec::NdxCodec::write_ndx_done(codec, &mut done_buf).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch NDX_DONE",
                    std::path::PathBuf::new(),
                    e,
                )
            })?;
            let mut writer_guard = batch_writer_arc.lock().unwrap();
            writer_guard.write_data(&done_buf).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch NDX_DONE",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })?;
        }

        Ok(())
    }

    /// Builds a mapping from traversal-order index to sorted-order index.
    ///
    /// Replicates upstream's `flist_sort_and_clean()` sort order on the
    /// entry names collected during traversal. Returns a Vec where
    /// `result[traversal_index] = sorted_index`.
    fn build_batch_sort_mapping(&self) -> Vec<i32> {
        let n = self.batch_entry_sort_data.len();
        if n == 0 {
            return Vec::new();
        }

        // Build sort keys matching protocol::flist::sort logic.
        // Each key: (index, name_bytes, is_dir)
        let mut indices: Vec<usize> = (0..n).collect();
        indices.sort_by(|&a, &b| {
            let (ref name_a, is_dir_a) = self.batch_entry_sort_data[a];
            let (ref name_b, is_dir_b) = self.batch_entry_sort_data[b];
            batch_entry_compare(name_a, is_dir_a, name_b, is_dir_b)
        });

        // indices[sorted_pos] = traversal_index
        // We need the inverse: traversal_to_sorted[traversal_index] = sorted_pos
        let mut traversal_to_sorted = vec![0i32; n];
        for (sorted_pos, &traversal_idx) in indices.iter().enumerate() {
            traversal_to_sorted[traversal_idx] = sorted_pos as i32;
        }

        traversal_to_sorted
    }

    /// Returns a mutable reference to the batch delta buffer file.
    ///
    /// Used by `flush_literal_chunk` and `copy_matched_block` to redirect
    /// token writes to the delta buffer instead of the batch writer.
    pub(super) fn batch_delta_writer(
        &mut self,
    ) -> Option<&mut io::Cursor<Vec<u8>>> {
        self.batch_delta_buf.as_mut()
    }

    /// Increments the batch flist index counter.
    ///
    /// Called after each flist entry is captured to the batch file.
    pub(super) fn increment_batch_flist_index(&mut self) {
        self.batch_flist_index += 1;
    }

    /// Records sort metadata for a batch flist entry.
    ///
    /// Stores the entry name and directory flag in traversal order so that
    /// `flush_batch_delta_to_batch` can compute the same sort order that
    /// upstream's `flist_sort_and_clean()` produces after reading the batch.
    pub(super) fn record_batch_entry_sort_data(&mut self, name: &[u8], is_dir: bool) {
        self.batch_entry_sort_data.push((name.to_vec(), is_dir));
    }

    /// Returns whether `--numeric-ids` is enabled.
    #[cfg(unix)]
    pub(super) const fn numeric_ids_enabled(&self) -> bool {
        self.options.numeric_ids_enabled()
    }
}

/// Compares two batch flist entries for sorting, matching upstream's
/// `flist.c:f_name_cmp()` semantics.
///
/// Rules:
/// 1. "." always sorts first (root directory marker)
/// 2. Files sort before directories at the same level
/// 3. Directories are compared as if they have a trailing '/'
/// 4. Within the same type, sort by unsigned byte comparison
fn batch_entry_compare(
    name_a: &[u8],
    is_dir_a: bool,
    name_b: &[u8],
    is_dir_b: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    // "." always comes first
    match (name_a == b".", name_b == b".") {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        (false, false) => {}
    }

    let last_slash_a = name_a.iter().rposition(|&b| b == b'/').unwrap_or(usize::MAX);
    let last_slash_b = name_b.iter().rposition(|&b| b == b'/').unwrap_or(usize::MAX);

    let mut i = 0;
    loop {
        let ch_a = if i < name_a.len() {
            name_a[i]
        } else if i == name_a.len() && is_dir_a {
            b'/'
        } else {
            0
        };

        let ch_b = if i < name_b.len() {
            name_b[i]
        } else if i == name_b.len() && is_dir_b {
            b'/'
        } else {
            0
        };

        let a_done = i > name_a.len() || (i == name_a.len() && !is_dir_a);
        let b_done = i > name_b.len() || (i == name_b.len() && !is_dir_b);

        if a_done && b_done {
            return Ordering::Equal;
        }
        if a_done {
            return Ordering::Less;
        }
        if b_done {
            return Ordering::Greater;
        }

        if ch_a != ch_b {
            let a_has_sep = last_slash_a != usize::MAX && last_slash_a >= i;
            let b_has_sep = last_slash_b != usize::MAX && last_slash_b >= i;

            let a_is_dir_here = a_has_sep || is_dir_a;
            let b_is_dir_here = b_has_sep || is_dir_b;

            match (a_is_dir_here, b_is_dir_here) {
                (true, false) => return Ordering::Greater,
                (false, true) => return Ordering::Less,
                _ => {}
            }

            return ch_a.cmp(&ch_b);
        }

        i += 1;
    }
}
