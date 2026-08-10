//! Parallel receive-side delta apply (#1368).
//!
//! Unconditionally compiled as of PFF-7. The design at
//! `docs/design/parallel-receive-delta-application.md` documents the
//! phased rollout that validated this path through PIP-9 and PIP-10.
//!
//! # Shape
//!
//! [`ParallelDeltaApplier`] owns a configurable concurrency limit and a
//! per-file map of [`std::sync::Mutex`]-guarded destination writers. Callers hand it
//! [`DeltaChunk`] values (one literal-or-block segment for one file) through
//! [`apply_one_chunk`](ParallelDeltaApplier::apply_one_chunk). The
//! checksum verify step runs on the rayon pool; the actual file-write happens
//! under the per-file mutex so per-file byte order is preserved.
//!
//! # Ordering preservation
//!
//! Two layers protect the wire-format invariants documented in section 2 of
//! the design doc:
//!
//! 1. **Per-file token order.** Each chunk carries a monotonic
//!    `chunk_sequence` per file. A per-file [`ReorderBuffer`] inside the
//!    applier replays chunks in submission order before they touch the
//!    destination writer, even though the rayon verify step completes out of
//!    order.
//! 2. **Per-file write exclusivity.** The destination writer for each file
//!    sits behind a [`std::sync::Mutex`], so only one chunk ever holds the writer at a
//!    time. Combined with the reorder buffer above, the bytes hit the file
//!    in the exact sequence the producer submitted them.
//!
//! Cross-file ordering at the wire-output layer is the
//! [`super::ReorderBuffer`] caller's responsibility (the existing
//! `DeltaConsumer` pattern already covers that case).
//!
//! # Module layout
//!
//! The applier was decomposed into cohesive submodules; this `mod.rs` is the
//! hub that wires them together and re-exports the previously module-visible
//! items so callers (`concurrent_delta::parallel_apply::...`) compile
//! unchanged.
//!
//! * `error` - typed [`ParallelApplyError`] and its `io::Error` bridge.
//! * `chunk` - public [`DeltaChunk`] segment plus the internal
//!   `VerifiedChunk`.
//! * `file_slot` - per-file writer + reorder ring (`FileSlot`) and the
//!   `IngestError` outcome.
//! * `handle` - `SlotHandle`, the per-slot RAII lock factory.
//! * `applier` - the [`ParallelDeltaApplier`] struct and its core
//!   register/apply/verify impl.
//! * `batch` - the batched `apply_batch_parallel` impl.
//! * `drain` - the `flush_workers` / `finish_file` drain primitives.
//! * `slot_barrier`, `decrement_guard` - per-slot synchronisation
//!   primitives. `ring_cap_env`, `shard_sizing` - construction-time env
//!   knobs.
//!
//! [`ReorderBuffer`]: super::reorder::ReorderBuffer

mod applier;
mod batch;
mod chunk;
mod decrement_guard;
mod drain;
mod error;
mod file_slot;
mod handle;
mod ring_cap_env;
mod shard_sizing;
mod slot_barrier;

pub use applier::ParallelDeltaApplier;
pub use chunk::DeltaChunk;
pub use error::ParallelApplyError;

// Re-exported at `parallel_apply` scope so the sibling submodules
// (`batch`, `drain`, `slot_barrier`) that reference these items by bare
// `super::X` paths keep resolving unchanged after the decomposition. The
// visibility matches each item's own `pub(in ...)` / `pub(super)` origin
// so the re-export never widens it.
pub(in crate::concurrent_delta::parallel_apply) use chunk::VerifiedChunk;
pub(in crate::concurrent_delta::parallel_apply) use file_slot::{FileSlot, IngestError};

#[cfg(test)]
pub(super) mod tests;

#[cfg(test)]
mod stress;
