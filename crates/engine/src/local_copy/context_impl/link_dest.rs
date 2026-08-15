// `--link-dest` / `--copy-dest` / `--compare-dest` candidate resolution.
// upstream: generator.c `try_dests_reg()` / `try_dests_non()`.
impl<'a> CopyContext<'a> {
    /// Searches `--link-dest` directories for the BEST file matching the source.
    ///
    /// upstream: generator.c:954-983 `try_dests_reg()` scans every basis dir and
    /// tracks the highest match_level (2 = data matches `quick_check_ok`, 3 = data
    /// and attributes both match `unchanged_attrs`), breaking early only on an
    /// exact (level-3) match. Returning the first data-only candidate instead
    /// would let an earlier match_level-2 basis (attrs differ) shadow a later
    /// exact one, forcing an unnecessary copy + attr reapply where upstream would
    /// hard-link the exact basis with no reapply. The caller re-derives the
    /// winning candidate's level to choose hard-link vs copy, so returning the
    /// best candidate is sufficient to mirror upstream.
    pub(super) fn link_dest_target(
        &self,
        relative: &Path,
        source: &Path,
        metadata: &fs::Metadata,
        size_only: bool,
        ignore_times: bool,
        checksum: bool,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        if self.options.link_dest_entries().is_empty() {
            return Ok(None);
        }

        let metadata_options = self.metadata_options();
        let preserve_xattrs = {
            #[cfg(all(unix, feature = "xattr"))]
            {
                self.options.preserve_xattrs()
            }
            #[cfg(not(all(unix, feature = "xattr")))]
            {
                false
            }
        };

        let mut best: Option<(PathBuf, u8)> = None;
        for entry in self.options.link_dest_entries() {
            let candidate = entry.resolve(self.destination_root(), relative);
            let candidate_metadata = match fs::metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "inspect link-dest candidate",
                        candidate,
                        error,
                    ));
                }
            };

            if !candidate_metadata.file_type().is_file() {
                continue;
            }

            if !should_skip_copy(CopyComparison {
                source_path: source,
                source: metadata,
                destination_path: candidate.as_path(),
                destination: &candidate_metadata,
                size_only,
                ignore_times,
                checksum,
                checksum_algorithm: self.options.checksum_algorithm(),
                modify_window: self.options.modify_window(),
                prefetched_match: None,
            }) {
                continue;
            }

            // At least match_level 2 (data matches); match_level 3 when the
            // preserved attributes also match, which the caller hard-links.
            let level = if crate::local_copy::reference_attrs_unchanged(
                &candidate,
                source,
                metadata,
                &metadata_options,
                preserve_xattrs,
            ) {
                3
            } else {
                2
            };

            if best.as_ref().is_none_or(|(_, best_level)| level > *best_level) {
                best = Some((candidate, level));
            }
            // upstream: generator.c:979 - an exact match ends the scan.
            if level == 3 {
                break;
            }
        }

        Ok(best.map(|(candidate, _)| candidate))
    }

    /// Locates a `--link-dest` basis symlink at `relative` that points at the
    /// same `target`.
    ///
    /// Returns the basis symlink path when a link-dest entry holds a symlink
    /// with a matching target, so the receiver can hard-link the symlink into
    /// place (`hL`) instead of recreating it.
    ///
    /// upstream: generator.c:1117-1134 try_dests_non() - LINK_DEST hard-links a
    /// matching symlink from the basis when CAN_HARDLINK_SYMLINK is supported.
    pub(super) fn link_dest_symlink_target(
        &self,
        relative: &Path,
        target: &Path,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        if self.options.link_dest_entries().is_empty() {
            return Ok(None);
        }

        for entry in self.options.link_dest_entries() {
            let candidate = entry.resolve(self.destination_root(), relative);
            let candidate_metadata = match fs::symlink_metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "inspect link-dest symlink",
                        candidate,
                        error,
                    ));
                }
            };

            if !candidate_metadata.file_type().is_symlink() {
                continue;
            }

            match fs::read_link(&candidate) {
                Ok(basis_target) if basis_target == target => return Ok(Some(candidate)),
                Ok(_) => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "read link-dest symlink",
                        candidate,
                        error,
                    ));
                }
            }
        }

        Ok(None)
    }

    /// Locates a `--link-dest` basis device or special file at `relative` that
    /// exactly matches the source node, returning its path so the receiver can
    /// hard-link it into place (`hD`/`hS` + blank) instead of recreating it.
    ///
    /// Mirrors upstream `generator.c:1064-1152` try_dests_non(): a `LINK_DEST`
    /// basis entry of the same file-type bucket (`FT_DEVICE`/`FT_SPECIAL`) whose
    /// device number (devices) or `_S_IFMT` (specials) matches
    /// (`generator.c:664-678` quick_check_ok) AND whose preserved attributes are
    /// unchanged (`generator.c:468-507` unchanged_attrs) reaches match_level 3
    /// and is hard-linked, itemizing as an exact match. `CAN_HARDLINK_SPECIAL`
    /// is defined on Linux, so devices and specials participate.
    #[cfg(unix)]
    pub(super) fn link_dest_special_target(
        &self,
        relative: &Path,
        metadata: &fs::Metadata,
        metadata_options: &MetadataOptions,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        if self.options.link_dest_entries().is_empty() {
            return Ok(None);
        }

        let source_type = metadata.file_type();
        let source_is_device = source_type.is_block_device() || source_type.is_char_device();
        let source_is_special = source_type.is_fifo() || source_type.is_socket();
        if !source_is_device && !source_is_special {
            return Ok(None);
        }

        let modify_window = self.options.modify_window();

        for entry in self.options.link_dest_entries() {
            let candidate = entry.resolve(self.destination_root(), relative);
            let candidate_metadata = match fs::symlink_metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "inspect link-dest special",
                        candidate,
                        error,
                    ));
                }
            };

            let cand_type = candidate_metadata.file_type();

            // upstream: generator.c:1076 - the basis must share the source's
            // file-type bucket, and generator.c:657-671 quick_check_ok compares
            // st_rdev (devices) or _S_IFMT (specials, i.e. fifo vs socket).
            if source_is_device {
                if !(cand_type.is_block_device() || cand_type.is_char_device()) {
                    continue;
                }
                if metadata.rdev() != candidate_metadata.rdev() {
                    continue;
                }
            } else if source_type.is_fifo() != cand_type.is_fifo()
                || source_type.is_socket() != cand_type.is_socket()
            {
                continue;
            }

            // upstream: generator.c:468-507 unchanged_attrs - preserved mtime,
            // perms and ownership must match for the match_level-3 hard-link.
            if metadata_options.times()
                && !mtimes_within_window(metadata, &candidate_metadata, modify_window)
            {
                continue;
            }
            if metadata_options.permissions()
                && (metadata.mode() & 0o7777) != (candidate_metadata.mode() & 0o7777)
            {
                continue;
            }
            if metadata_options.owner() && metadata.uid() != candidate_metadata.uid() {
                continue;
            }
            if metadata_options.group() && metadata.gid() != candidate_metadata.gid() {
                continue;
            }

            return Ok(Some(candidate));
        }

        Ok(None)
    }

    /// Non-Unix stub: device and special nodes cannot be materialised, so no
    /// `--link-dest` basis can ever match one.
    #[cfg(not(unix))]
    pub(super) fn link_dest_special_target(
        &self,
        _relative: &Path,
        _metadata: &fs::Metadata,
        _metadata_options: &MetadataOptions,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        Ok(None)
    }

}

/// Returns `true` when two nodes' modification times are equal within
/// `--modify-window`.
///
/// upstream: util1.c:1478 same_time() - a whole-second delta within
/// `modify_window` counts as unchanged. For a zero window (the default) or a
/// negative window (`--modify-window < 0`, nsec-exact) a hardlink candidate
/// must match on both the whole-second and nanosecond components.
#[cfg(unix)]
fn mtimes_within_window(
    source: &fs::Metadata,
    candidate: &fs::Metadata,
    modify_window: ModifyWindow,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    let delta = source.mtime().abs_diff(candidate.mtime());
    let window = modify_window.as_secs();
    if window <= 0 {
        delta == 0 && source.mtime_nsec() == candidate.mtime_nsec()
    } else {
        delta <= window as u64
    }
}
