//! Test submodules for the buffer pool.
//!
//! The pool's behavioural surface is broad (basic ops, adaptive sizing,
//! contention, TLS cache, byte budget, memory cap, throughput tracker,
//! buffer controller, adaptive resizing, telemetry, per-thread slab) so
//! the tests are partitioned by concern. Each submodule focuses on a
//! single facet to keep individual files reviewable and below the
//! workspace LoC cap.

mod support;

mod adaptive_pool;
mod byte_budget;
mod contention;
mod controller;
mod memory_cap;
mod pool_basic;
mod telemetry;
mod thread_cache;
mod throughput;

#[cfg(feature = "thread-slab-pool")]
mod slab;
