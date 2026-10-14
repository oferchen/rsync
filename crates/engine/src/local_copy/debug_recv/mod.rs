//! DEBUG_RECV tracing for receiver operations.
//!
//! Provides structured tracing for file receive/delta-apply operations, the
//! counterpart of upstream's `DEBUG_GTE(RECV, ...)` output in `receiver.c`.
//! All tracing is conditionally compiled behind the `tracing` feature flag and
//! produces no-op inline functions when disabled.
//!
//! Deviation from upstream: events are structured `tracing` records, not
//! upstream's `rprintf(FINFO, ...)` text lines.

#![allow(dead_code)]

mod trace_functions;
mod tracer;

pub use trace_functions::{
    trace_basis_file_selected, trace_checksum_verify, trace_delta_apply_end,
    trace_delta_apply_literal, trace_delta_apply_match, trace_delta_apply_start,
    trace_recv_file_end, trace_recv_file_start, trace_recv_summary,
};
pub use tracer::RecvTracer;

/// Target name for tracing events, named after upstream's `RECV` debug category.
const RECV_TARGET: &str = "rsync::recv";

#[cfg(test)]
mod tests;
