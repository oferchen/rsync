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
//!   delete in one directory. Plan order matches upstream
//!   `delete_in_dir`'s reverse iteration of `compare_file_entries`
//!   ascending order.
//! - [`DeletePlanMap`] - a concurrent map keyed by destination-relative
//!   directory path. Workers publish [`DeletePlan`] values into the map
//!   from rayon segment-dispatch threads; the single emitter pulls them
//!   out in upstream traversal order.
//! - [`DirTraversalCursor`] - yields directories in upstream's depth-first,
//!   `f_name_cmp`-ascending order. Backed by a tree built from observed
//!   flist segments.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-387`
//!   (`delete_in_dir`, `do_delete_pass`).
//! - `target/interop/upstream-src/rsync-3.4.1/flist.c:3217-3343`
//!   (`f_name_cmp`).

mod plan;
mod plan_map;
mod traversal;

pub use plan::{DeleteEntry, DeleteEntryKind, DeletePlan, HardlinkCohortId};
pub use plan_map::DeletePlanMap;
pub use traversal::DirTraversalCursor;
