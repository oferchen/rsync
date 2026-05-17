//! Data structures for the parallel-deterministic-delete pipeline.
//!
//! This module hosts the foundational types described in
//! `docs/design/parallel-deterministic-delete.md` (PR #4257). The data
//! structures here are added in isolation; the receiver and emitter wiring
//! that consumes them lands in later tasks in the DDP series.
//!
//! # Components
//!
//! - [`DeletePlan`] - a sorted, frozen list of destination entries to
//!   delete in one directory.
//! - [`DeletePlanMap`] - a concurrent map keyed by destination-relative
//!   directory path.
//! - [`DirTraversalCursor`] - yields directories in upstream's depth-first,
//!   `f_name_cmp`-ascending order.
//! - [`emitter`] - single-threaded drain that consumes [`DeletePlanMap`]
//!   entries in [`DirTraversalCursor`] order.
//! - [`CohortIndex`] - read-only hardlink cohort snapshot built per
//!   INC_RECURSE segment.
//! - [`DeleteContext`] - receiver-side shared state that owns the
//!   [`DeletePlanMap`] + [`DirTraversalCursor`] and exposes
//!   `observe_segment_for_delete` to publish a [`DeletePlan`] per
//!   incoming INC_RECURSE segment.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-387`
//!   (`delete_in_dir`, `do_delete_pass`).
//! - `target/interop/upstream-src/rsync-3.4.1/delete.c:82-225`
//!   (`delete_item`).
//! - `target/interop/upstream-src/rsync-3.4.1/flist.c:3217-3343`
//!   (`f_name_cmp`).

mod cohort_index;
mod context;
pub mod emitter;
mod extras;
mod plan;
mod plan_map;
mod traversal;

pub use cohort_index::CohortIndex;
pub use context::{DeleteContext, DrainOutcome, EmitterTiming};
pub use emitter::{
    CohortDeleteRecord, DeleteEmitter, DeleteEvent, DeleteFs, EMITTER_PARTIAL_EXIT_CODE,
    EMITTER_VANISHED_EXIT_CODE, EmitterErrorPolicy, RealDeleteFs, RecordingDeleteFs,
};
pub use extras::{compute_extras, compute_extras_with_cohorts};
pub use plan::{DeleteEntry, DeleteEntryKind, DeletePlan, HardlinkCohortId};
pub use plan_map::DeletePlanMap;
pub use traversal::DirTraversalCursor;
