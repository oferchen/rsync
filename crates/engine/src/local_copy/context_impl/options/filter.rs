impl<'a> CopyContext<'a> {
    /// Returns the filter program used by xattr sync logic.
    #[cfg(all(any(unix, windows), feature = "xattr"))]
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

    /// Freezes the effective deletion filter chain for the current directory
    /// into an immutable, `Send + Sync` [`DeletionFilterSnapshot`].
    ///
    /// Captures the global filter program plus the live per-directory merge
    /// state (static layers, the active ephemeral frame, and the top dynamic
    /// `dir-merge` frame) by value, so the per-entry `allows_deletion` decision
    /// can be evaluated off-thread without ever touching the `Rc<RefCell<...>>`
    /// filter stacks. Read-only: the live stacks are cloned, never mutated.
    ///
    /// Call this AFTER [`Self::enter_destination_for_deletion`] has seeded the
    /// destination-side per-dir merge rules so the snapshot reflects the exact
    /// chain the serial path would evaluate against.
    pub(crate) fn deletion_filter_snapshot(&self) -> DeletionFilterSnapshot {
        let dynamic_loaded_segments = self
            .dynamic_dir_merge_stack
            .borrow()
            .last()
            .map(|frame| frame.loaded_segments.clone())
            .unwrap_or_default();
        DeletionFilterSnapshot {
            program: self.filter_program.clone(),
            layers: self.dir_merge_layers.borrow().clone(),
            ephemeral_last: self.dir_merge_ephemeral.borrow().last().cloned(),
            dynamic_loaded_segments,
            filter_set: self.options.filter_set().cloned(),
            delete_excluded: self.options.delete_excluded_enabled(),
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

/// Immutable, `Send + Sync` snapshot of the effective deletion filter chain
/// for a single destination directory.
///
/// The per-entry deletion decision is a pure function of the candidate path,
/// its file kind, and the directory's frozen filter chain - upstream evaluates
/// it serially in `delete.c:delete_in_dir()`, but the decision itself carries
/// no order dependence. This snapshot lets the local-copy `--delete` DECIDE
/// phase compute that pure function across rayon worker threads while leaving
/// the live `Rc<RefCell<...>>` filter stacks untouched on the owning thread.
///
/// [`Self::allows_deletion`] mirrors [`CopyContext::allows_deletion`]
/// instruction-for-instruction; parallelism only changes WHERE the decision
/// runs, never WHAT it decides.
#[derive(Clone)]
pub(crate) struct DeletionFilterSnapshot {
    program: Option<FilterProgram>,
    layers: FilterSegmentLayers,
    ephemeral_last: Option<Vec<(usize, FilterSegment)>>,
    dynamic_loaded_segments: Vec<LoadedDynamicSegment>,
    filter_set: Option<filters::FilterSet>,
    delete_excluded: bool,
}

impl DeletionFilterSnapshot {
    /// Returns `true` when the entry at `relative` may be deleted, evaluated
    /// against the frozen chain. Byte-for-byte equivalent to
    /// [`CopyContext::allows_deletion`].
    pub(crate) fn allows_deletion(&self, relative: &Path, is_dir: bool) -> bool {
        let delete_excluded = self.delete_excluded;
        if let Some(program) = &self.program {
            if let Some(outcome) = self.evaluate_dynamic_segments(relative, is_dir)
                && outcome.transfer_decided()
            {
                return if delete_excluded {
                    outcome.allows_deletion() || outcome.allows_deletion_when_excluded_removed()
                } else {
                    outcome.allows_deletion()
                };
            }
            let temp_layers = self.ephemeral_last.as_deref();
            let outcome = program.evaluate(
                relative,
                is_dir,
                self.layers.as_slice(),
                temp_layers,
                FilterContext::Deletion,
            );
            if delete_excluded {
                outcome.allows_deletion() || outcome.allows_deletion_when_excluded_removed()
            } else {
                outcome.allows_deletion()
            }
        } else if let Some(filters) = &self.filter_set {
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

    /// Snapshot equivalent of [`CopyContext::evaluate_dynamic_segments`] for
    /// the deletion context. Returns `None` when no dynamic segments are active
    /// (no frame, or an empty frame), matching the live path's None cases.
    fn evaluate_dynamic_segments(&self, relative: &Path, is_dir: bool) -> Option<FilterOutcome> {
        if self.dynamic_loaded_segments.is_empty() {
            return None;
        }
        let mut outcome = FilterOutcome::default();
        for loaded in self.dynamic_loaded_segments.iter().rev() {
            if outcome.transfer_decided() {
                break;
            }
            loaded
                .segment
                .apply(relative, is_dir, &mut outcome, FilterContext::Deletion);
        }
        Some(outcome)
    }
}

// Compile-time guarantee that the snapshot can cross a rayon boundary. The
// XMP invariant requires the per-entry matcher to capture ONLY immutable
// Send + Sync data; this assertion fails the build if any field ever
// reintroduces an Rc/RefCell.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DeletionFilterSnapshot>();
};
