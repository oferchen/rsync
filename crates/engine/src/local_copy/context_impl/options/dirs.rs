impl<'a> CopyContext<'a> {
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
                // upstream: generator.c:1329-1333 - a missing leading parent
                // is materialized on demand (make_path) when creation is
                // allowed, or under `relative_paths && !implied_dirs`; in
                // dry-run mode the real mkdir is elided, so mirror the real
                // run's acceptance of the missing parent in both cases.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if allow_creation || self.relative_paths_enabled() {
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
                // upstream: generator.c:1329-1333 - with `--no-implied-dirs`
                // the implied parent dirs are absent from the flist, but under
                // `relative_paths && !implied_dirs` recv_generator() still
                // materializes a missing leading dir on demand via
                // `make_path(fname, MKP_DROP_NAME | ...)`. The dir is created
                // with default attributes (no source-attr mirroring); that
                // suppression happens in retouch_relative_implied_dirs, which
                // is already gated on implied_dirs_enabled(). Outside
                // --relative mode there are no implied dirs, so a missing
                // parent remains an error, matching upstream.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if self.relative_paths_enabled() {
                        fs::create_dir_all(parent).map_err(|error| {
                            LocalCopyError::io("create parent directory", parent, error)
                        })?;
                        self.register_progress();
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
}
