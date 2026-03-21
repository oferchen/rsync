//! DEBUG_DELTASUM tracing for delta/checksum operations.
//!
//! Provides structured tracing for delta matching and checksum generation that
//! matches upstream rsync's `match.c`/`checksum.c` debug output format. All
//! tracing is conditionally compiled behind the `tracing` feature flag and
//! produces no-op inline functions when disabled.

#![allow(dead_code)]

mod checksum;
mod matching;
mod tracer;

/// Target name for tracing events, matching rsync's debug category.
const DELTASUM_TARGET: &str = "rsync::deltasum";

pub use checksum::{trace_checksum_block, trace_checksum_end, trace_checksum_start};
pub use matching::{
    trace_deltasum_summary, trace_match_end, trace_match_false_alarm, trace_match_hit,
    trace_match_miss, trace_match_start,
};
pub use tracer::DeltasumTracer;
