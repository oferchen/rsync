//! [`DeleteContext`] - per-transfer wiring that ties the flist segment
//! consumer to the parallel-deterministic-delete pipeline and drives the
//! [`DeleteEmitter`](super::emitter::DeleteEmitter) for every `--delete-*`
//! timing mode.
//!
//! This module unifies two responsibilities introduced across the DDP
//! task series:
//!
//! 1. The receiver-side observation API
//!    ([`DeleteContext::observe_segment_for_delete`]) landed in DDP-B3.
//!    The receiver calls it once per INC_RECURSE segment; the context
//!    resolves the destination directory, computes per-directory extras
//!    via [`compute_extras`](super::extras::compute_extras), publishes a
//!    sorted [`DeletePlan`](super::plan::DeletePlan) into the shared
//!    [`DeletePlanMap`](super::plan_map::DeletePlanMap), and records
//!    child directories with the
//!    [`DirTraversalCursor`](super::traversal::DirTraversalCursor) so
//!    the emitter can yield directories in upstream `f_name_cmp`
//!    ascending order.
//! 2. The timing-mode drain API (DDP-E1-E5). Each `--delete-*` timing
//!    mode keeps the observable semantics it had under the legacy
//!    batched-sweep path, but every unlink, itemize line, and stats
//!    counter now flows through the single-threaded
//!    [`DeleteEmitter`](super::emitter::DeleteEmitter) drain.
//!
//! # Wiring per timing mode
//!
//! | Mode             | Phase 1 (plan publish)                | Phase 2 (drain)                                      |
//! |------------------|---------------------------------------|------------------------------------------------------|
//! | `--delete-before`| pre-walk pass over every dir          | [`DeleteContext::emit_all`] before the copy walk     |
//! | `--delete-during`| per-dir inside the copy walk          | [`DeleteContext::emit_one`] before the dir's copies  |
//! | `--delete-after` | per-dir inside the copy walk          | [`DeleteContext::emit_all`] after the copy walk      |
//! | `--delete-delay` | per-dir inside the copy walk          | [`DeleteContext::emit_all`] after all renames commit |
//! | `--delete-excluded` (layered) | upstream of `compute_extras` - filter-excluded entries are appended to the segment-extras set | per timing mode above |
//!
//! The legacy batched sweep was retired in DDP-F3 (#2272); the emitter
//! is now the sole production unlink path for every timing mode.
//!
//! # Concurrency
//!
//! [`DeletePlanMap`](super::plan_map::DeletePlanMap) already provides
//! interior mutability via a global mutex; the traversal cursor is
//! wrapped in a [`Mutex`](std::sync::Mutex) here. The observation API
//! takes `&self`, so the context can live inside an
//! [`Arc`](std::sync::Arc) shared between the receiver and worker
//! threads. The drain consumes the context by value (`mut self`) and is
//! therefore the single-writer path that owns the emitter.
//!
//! # Submodules
//!
//! - `core` - the [`DeleteContext`] struct, its constructors, the
//!   observation API, and the `emit_*`/`into_emitter` drain plumbing.
//! - `outcome` - the [`DrainOutcome`] returned by every `emit_*` call.
//! - `timing` - the [`EmitterTiming`] enum and its conversions to/from
//!   [`crate::local_copy::DeleteTiming`].

mod core;
mod outcome;
mod timing;

#[cfg(test)]
mod tests;

pub use core::DeleteContext;
pub use outcome::DrainOutcome;
pub use timing::EmitterTiming;
