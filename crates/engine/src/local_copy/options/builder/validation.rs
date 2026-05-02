//! Validation logic and final build methods.

use super::LocalCopyOptionsBuilder;
use super::error::BuilderError;
use crate::local_copy::options::types::{LocalCopyOptions, ReferenceDirectoryKind};

impl LocalCopyOptionsBuilder {
    /// Validates the builder configuration and returns any errors.
    fn validate(&self) -> Result<(), BuilderError> {
        // size_only and checksum are mutually exclusive
        if self.size_only && self.checksum {
            return Err(BuilderError::ConflictingOptions {
                option1: "size_only",
                option2: "checksum",
            });
        }

        // inplace and delay_updates are mutually exclusive
        if self.inplace && self.delay_updates {
            return Err(BuilderError::ConflictingOptions {
                option1: "inplace",
                option2: "delay_updates",
            });
        }

        // Validate size limits
        if let (Some(min), Some(max)) = (self.min_file_size, self.max_file_size) {
            if min > max {
                return Err(BuilderError::InvalidCombination {
                    message: format!(
                        "min_file_size ({min}) cannot be greater than max_file_size ({max})"
                    ),
                });
            }
        }

        // copy_links and preserve_symlinks are mutually exclusive
        if self.copy_links && self.preserve_symlinks {
            return Err(BuilderError::ConflictingOptions {
                option1: "copy_links",
                option2: "preserve_symlinks",
            });
        }

        // --compare-dest, --copy-dest, and --link-dest are mutually exclusive
        // (upstream rsync enforces this). Multiple directories of the same kind
        // are allowed, but mixing kinds is not.
        {
            let mut has_compare = false;
            let mut has_copy = false;
            let mut has_link = false;
            for reference in &self.reference_directories {
                match reference.kind() {
                    ReferenceDirectoryKind::Compare => has_compare = true,
                    ReferenceDirectoryKind::Copy => has_copy = true,
                    ReferenceDirectoryKind::Link => has_link = true,
                }
            }
            let kind_count = has_compare as u8 + has_copy as u8 + has_link as u8;
            if kind_count > 1 {
                let first = if has_compare {
                    "--compare-dest"
                } else {
                    "--copy-dest"
                };
                let second = if has_link {
                    "--link-dest"
                } else {
                    "--copy-dest"
                };
                return Err(BuilderError::ConflictingOptions {
                    option1: first,
                    option2: second,
                });
            }
        }

        Ok(())
    }

    /// Builds the [`LocalCopyOptions`] with validation.
    ///
    /// # Errors
    ///
    /// Returns a [`BuilderError`] if the configuration is invalid.
    pub fn build(self) -> Result<LocalCopyOptions, BuilderError> {
        self.validate()?;
        Ok(self.build_unchecked())
    }

    /// Builds the [`LocalCopyOptions`] without validation.
    ///
    /// This is useful when you know the configuration is valid or want
    /// to skip validation for performance reasons.
    #[must_use]
    pub fn build_unchecked(self) -> LocalCopyOptions {
        LocalCopyOptions {
            delete: self.delete,
            delete_timing: self.delete_timing,
            delete_excluded: self.delete_excluded,
            delete_missing_args: self.delete_missing_args,
            max_deletions: self.max_deletions,
            min_file_size: self.min_file_size,
            max_file_size: self.max_file_size,
            block_size_override: self.block_size_override,
            remove_source_files: self.remove_source_files,
            preallocate: self.preallocate,
            fsync: self.fsync,
            bandwidth_limit: self.bandwidth_limit,
            bandwidth_burst: self.bandwidth_burst,
            compress: self.compress,
            compression_algorithm: self.compression_algorithm,
            compression_level_override: self.compression_level_override,
            compression_level: self.compression_level,
            skip_compress: self.skip_compress,
            open_noatime: self.open_noatime,
            whole_file: self.whole_file,
            copy_links: self.copy_links,
            preserve_symlinks: self.preserve_symlinks,
            copy_dirlinks: self.copy_dirlinks,
            copy_unsafe_links: self.copy_unsafe_links,
            keep_dirlinks: self.keep_dirlinks,
            safe_links: self.safe_links,
            munge_links: self.munge_links,
            preserve_owner: self.preserve_owner,
            preserve_group: self.preserve_group,
            preserve_executability: self.preserve_executability,
            preserve_permissions: self.preserve_permissions,
            preserve_times: self.preserve_times,
            preserve_atimes: self.preserve_atimes,
            preserve_crtimes: self.preserve_crtimes,
            omit_link_times: self.omit_link_times,
            owner_override: self.owner_override,
            group_override: self.group_override,
            copy_as: self.copy_as,
            omit_dir_times: self.omit_dir_times,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls: self.preserve_acls,
            filters: self.filters,
            filter_program: self.filter_program,
            iconv: self.iconv,
            numeric_ids: self.numeric_ids,
            sparse: self.sparse,
            checksum: self.checksum,
            checksum_algorithm: self.checksum_algorithm,
            checksum_seed: self.checksum_seed,
            size_only: self.size_only,
            ignore_times: self.ignore_times,
            ignore_existing: self.ignore_existing,
            existing_only: self.existing_only,
            ignore_missing_args: self.ignore_missing_args,
            update: self.update,
            modify_window: self.modify_window,
            partial: self.partial,
            partial_dir: self.partial_dir,
            temp_dir: self.temp_dir,
            delay_updates: self.delay_updates,
            inplace: self.inplace,
            append: self.append,
            append_verify: self.append_verify,
            collect_events: self.collect_events,
            preserve_hard_links: self.preserve_hard_links,
            relative_paths: self.relative_paths,
            one_file_system: self.one_file_system,
            recursive: self.recursive,
            dirs: self.dirs,
            devices: self.devices,
            copy_devices_as_files: self.copy_devices_as_files,
            specials: self.specials,
            force_replacements: self.force_replacements,
            implied_dirs: self.implied_dirs,
            mkpath: self.mkpath,
            prune_empty_dirs: self.prune_empty_dirs,
            timeout: self.timeout,
            contimeout: self.contimeout,
            stop_at: self.stop_at,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs: self.preserve_xattrs,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_nfsv4_acls: self.preserve_nfsv4_acls,
            backup: self.backup,
            backup_dir: self.backup_dir,
            backup_suffix: self.backup_suffix,
            link_dests: self.link_dests,
            reference_directories: self.reference_directories,
            chmod: self.chmod,
            user_mapping: self.user_mapping,
            group_mapping: self.group_mapping,
            batch_writer: self.batch_writer,
            super_mode: self.super_mode,
            fake_super: self.fake_super,
            ignore_errors: self.ignore_errors,
            log_file: self.log_file,
            log_file_format: self.log_file_format,
            platform_copy: self.platform_copy,
        }
    }
}

impl LocalCopyOptions {
    /// Creates a new [`LocalCopyOptionsBuilder`] for constructing options.
    ///
    /// # Example
    ///
    /// ```rust
    /// use engine::local_copy::LocalCopyOptions;
    ///
    /// let options = LocalCopyOptions::builder()
    ///     .recursive(true)
    ///     .preserve_times(true)
    ///     .build()
    ///     .expect("valid options");
    /// ```
    #[must_use]
    pub fn builder() -> LocalCopyOptionsBuilder {
        LocalCopyOptionsBuilder::new()
    }
}
