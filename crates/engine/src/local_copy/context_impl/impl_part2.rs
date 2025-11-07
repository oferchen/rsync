use std::num::NonZeroU32;

impl<'a> CopyContext<'a> {
    pub(super) fn metadata_options(&self) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_owner(self.options.preserve_owner())
            .preserve_group(self.options.preserve_group())
            .preserve_permissions(self.options.preserve_permissions())
            .preserve_times(self.options.preserve_times())
            .numeric_ids(self.options.numeric_ids_enabled())
            .with_owner_override(self.options.owner_override())
            .with_group_override(self.options.group_override())
            .with_chmod(self.options.chmod().cloned())
            .with_user_mapping(self.options.user_mapping().cloned())
            .with_group_mapping(self.options.group_mapping().cloned())
    }

    pub(super) fn copy_links_enabled(&self) -> bool {
        self.options.copy_links_enabled()
    }

    pub(super) fn copy_unsafe_links_enabled(&self) -> bool {
        self.options.copy_unsafe_links_enabled()
    }

    pub(super) fn safe_links_enabled(&self) -> bool {
        self.options.safe_links_enabled()
    }

    pub(super) fn copy_dirlinks_enabled(&self) -> bool {
        self.options.copy_dirlinks_enabled()
    }

    pub(super) fn keep_dirlinks_enabled(&self) -> bool {
        self.options.keep_dirlinks_enabled()
    }

    pub(super) fn whole_file_enabled(&self) -> bool {
        self.options.whole_file_enabled()
    }

    pub(super) fn sparse_enabled(&self) -> bool {
        self.options.sparse_enabled()
    }

    pub(super) fn append_enabled(&self) -> bool {
        self.options.append_enabled()
    }

    pub(super) fn append_verify_enabled(&self) -> bool {
        self.options.append_verify_enabled()
    }

    pub(super) fn preallocate_enabled(&self) -> bool {
        self.options.preallocate_enabled()
    }

    pub(super) fn devices_enabled(&self) -> bool {
        self.options.devices_enabled()
    }

    pub(super) fn specials_enabled(&self) -> bool {
        self.options.specials_enabled()
    }

    pub(super) fn filter_program(
        &self,
    ) -> Option<&crate::local_copy::filter_program::FilterProgram> {
        self.filter_program.as_ref()
    }

    #[cfg(feature = "acl")]
    pub(super) fn acls_enabled(&self) -> bool {
        self.options.acls_enabled()
    }

    pub(super) fn relative_paths_enabled(&self) -> bool {
        self.options.relative_paths_enabled()
    }

    pub(super) fn recursive_enabled(&self) -> bool {
        self.options.recursive_enabled()
    }

    pub(super) fn dirs_enabled(&self) -> bool {
        self.options.dirs_enabled()
    }

    pub(super) fn implied_dirs_enabled(&self) -> bool {
        self.options.implied_dirs_enabled()
    }

    pub(super) fn mkpath_enabled(&self) -> bool {
        self.options.mkpath_enabled()
    }

    pub(super) fn prune_empty_dirs_enabled(&self) -> bool {
        self.options.prune_empty_dirs_enabled()
    }

    pub(super) fn omit_dir_times_enabled(&self) -> bool {
        self.options.omit_dir_times_enabled()
    }

    pub(super) fn omit_link_times_enabled(&self) -> bool {
        self.options.omit_link_times_enabled()
    }

    pub(super) fn prepare_parent_directory(&mut self, parent: &Path) -> Result<(), LocalCopyError> {
        if parent.as_os_str().is_empty() {
            return Ok(());
        }

        let allow_creation = self.implied_dirs_enabled() || self.mkpath_enabled();
        let keep_dirlinks = self.keep_dirlinks_enabled();

        if self.mode.is_dry_run() {
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
                                Err(LocalCopyError::invalid_argument(
                                    LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                                ))
                            }
                        })
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
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
                            Err(LocalCopyError::invalid_argument(
                                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                            ))
                        }
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir_all(parent).map_err(|error| {
                        LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
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
                            Err(LocalCopyError::invalid_argument(
                                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                            ))
                        }
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
                    }
                }
                Err(error) => Err(LocalCopyError::io(
                    "create parent directory",
                    parent.to_path_buf(),
                    error,
                )),
            }
        }
    }

    pub(super) fn remove_source_files_enabled(&self) -> bool {
        self.options.remove_source_files_enabled()
    }

    pub(super) fn compress_enabled(&self) -> bool {
        self.options.compress_enabled()
    }

    pub(super) fn should_compress(&self, relative: &Path) -> bool {
        self.compress_enabled() && !self.options.should_skip_compress(relative)
    }

    pub(super) fn compression_level(&self) -> CompressionLevel {
        self.options.compression_level()
    }

    pub(super) fn compression_algorithm(&self) -> CompressionAlgorithm {
        self.options.compression_algorithm()
    }

    pub(super) fn block_size_override(&self) -> Option<NonZeroU32> {
        self.options.block_size_override()
    }

    pub(super) fn checksum_enabled(&self) -> bool {
        self.options.checksum_enabled()
    }

    pub(super) fn size_only_enabled(&self) -> bool {
        self.options.size_only_enabled()
    }

    pub(super) fn ignore_existing_enabled(&self) -> bool {
        self.options.ignore_existing_enabled()
    }

    pub(super) fn ignore_missing_args_enabled(&self) -> bool {
        self.options.ignore_missing_args_enabled()
    }

    pub(super) fn update_enabled(&self) -> bool {
        self.options.update_enabled()
    }

    pub(super) fn partial_enabled(&self) -> bool {
        self.options.partial_enabled()
    }

    pub(super) fn partial_directory_path(&self) -> Option<&Path> {
        self.options.partial_directory_path()
    }

    pub(super) fn temp_directory_path(&self) -> Option<&Path> {
        self.options.temp_directory_path()
    }

    pub(super) fn inplace_enabled(&self) -> bool {
        self.options.inplace_enabled()
    }

    #[cfg(feature = "xattr")]
    pub(super) fn xattrs_enabled(&self) -> bool {
        self.options.preserve_xattrs()
    }

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

}
