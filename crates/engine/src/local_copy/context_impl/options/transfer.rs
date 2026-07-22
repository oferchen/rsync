use std::num::NonZeroU32;

impl<'a> CopyContext<'a> {
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

    pub(super) const fn copy_dirlinks_enabled(&self) -> bool {
        self.options.copy_dirlinks_enabled()
    }

    pub(super) const fn keep_dirlinks_enabled(&self) -> bool {
        self.options.keep_dirlinks_enabled()
    }

    pub(super) const fn whole_file_enabled(&self) -> bool {
        self.options.whole_file_enabled()
    }

    /// Returns whether the active copy-on-write policy permits reflink
    /// acceleration in the delta COPY-token path.
    ///
    /// REFLINK-4: `--no-cow` / `--reflink=never` installs a platform-copy
    /// strategy whose `supports_reflink()` is `false`, and `--cow` /
    /// `--reflink=always|auto` leaves a strategy that reports `true`. The
    /// delta path consults this before attempting `FICLONERANGE`, so the same
    /// flag that gates whole-file `FICLONE` (REFLINK-2) also gates range
    /// clones.
    pub(super) fn reflink_enabled(&self) -> bool {
        self.options.platform_copy().supports_reflink()
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

    /// Returns the readable byte length to use when `--copy-devices` should
    /// stream `source` (a block/char device) as a regular file, or `None` when
    /// `source` is not a copy-devices device and the caller should use the stat
    /// length as usual.
    ///
    /// Mirrors upstream `flist.c:1451-1456 make_file()`, which opens the device
    /// and records `get_device_size()` in place of the (zero) stat length. See
    /// [`crate::local_copy::LocalCopyMetadata::virtualize_copy_device_as_file`]
    /// for the matching reporting override.
    pub(super) fn copy_device_as_file_size(
        &self,
        source: &std::path::Path,
        metadata: &std::fs::Metadata,
    ) -> Option<u64> {
        if !self.copy_devices_as_files_enabled()
            || !crate::local_copy::is_device(metadata.file_type())
        {
            return None;
        }
        #[cfg(unix)]
        {
            Some(::metadata::device_readable_size(source).unwrap_or(0))
        }
        #[cfg(not(unix))]
        {
            let _ = source;
            None
        }
    }

    pub(super) const fn specials_enabled(&self) -> bool {
        self.options.specials_enabled()
    }

    pub(super) const fn list_only_enabled(&self) -> bool {
        self.options.list_only_enabled()
    }

    pub(super) const fn force_replacements_enabled(&self) -> bool {
        self.options.force_replacements_enabled()
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

    pub(super) const fn compression_threads(&self) -> Option<std::num::NonZeroU8> {
        self.options.compression_threads()
    }

    pub(super) const fn block_size_override(&self) -> Option<NonZeroU32> {
        self.options.block_size_override()
    }

    pub(super) const fn fuzzy_level_enabled(&self) -> u8 {
        self.options.fuzzy_level_enabled()
    }

    pub(super) const fn checksum_enabled(&self) -> bool {
        self.options.checksum_enabled()
    }

    pub(super) const fn xxh64_dedup_enabled(&self) -> bool {
        self.options.xxh64_dedup_enabled()
    }

    pub(super) const fn xxh64_dedup_size_limit(&self) -> u64 {
        self.options.xxh64_dedup_size_limit()
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
}
