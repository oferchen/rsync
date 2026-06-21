impl<'a> CopyContext<'a> {
    /// Returns the filter program used by xattr sync logic.
    #[cfg(all(unix, feature = "xattr"))]
    pub(super) const fn filter_program(
        &self,
    ) -> Option<&crate::local_copy::filter_program::FilterProgram> {
        self.filter_program.as_ref()
    }

    /// Evaluates filter rules to determine whether the entry is allowed for
    /// transfer. Returns `true` if the entry passes all filters.
    pub(super) fn allows(&self, relative: &Path, is_dir: bool) -> bool {
        if let Some(program) = &self.filter_program {
            if let Some(outcome) =
                self.evaluate_dynamic_segments(relative, is_dir, FilterContext::Transfer)
                && outcome.transfer_decided()
            {
                return outcome.allows_transfer();
            }

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
            if let Some(result) = self.dynamic_excluded_dir_by_non_dir_rule(relative) {
                return result;
            }
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
            if let Some(outcome) =
                self.evaluate_dynamic_segments(relative, is_dir, FilterContext::Deletion)
                && outcome.transfer_decided()
            {
                return if delete_excluded {
                    outcome.allows_deletion() || outcome.allows_deletion_when_excluded_removed()
                } else {
                    outcome.allows_deletion()
                };
            }
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

    /// Applies the top-of-stack dynamic per-directory merge segments against
    /// `relative` using upstream's first-match-wins semantics.
    ///
    /// upstream: exclude.c:check_filter() - local child rules precede inherited
    /// parent rules. Dynamic per-directory rules registered by a parent merge
    /// file fire BEFORE the parent's own static rules so that a `.filt2` rule
    /// in a subdirectory overrides any conflicting rule inherited from
    /// `bar/.filt`.
    fn evaluate_dynamic_segments(
        &self,
        relative: &Path,
        is_dir: bool,
        context: FilterContext,
    ) -> Option<FilterOutcome> {
        let stack = self.dynamic_dir_merge_stack.borrow();
        let frame = stack.last()?;
        if frame.loaded_segments.is_empty() {
            return None;
        }
        let mut outcome = FilterOutcome::default();
        for loaded in frame.loaded_segments.iter().rev() {
            if outcome.transfer_decided() {
                break;
            }
            loaded
                .segment
                .apply(relative, is_dir, &mut outcome, context);
        }
        Some(outcome)
    }

    fn dynamic_excluded_dir_by_non_dir_rule(&self, relative: &Path) -> Option<bool> {
        let stack = self.dynamic_dir_merge_stack.borrow();
        let frame = stack.last()?;
        for loaded in frame.loaded_segments.iter().rev() {
            if let Some(result) = loaded.segment.excluded_dir_by_non_dir_rule(relative) {
                return Some(result);
            }
        }
        None
    }
}
