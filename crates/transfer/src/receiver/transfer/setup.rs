//! Common transfer setup shared by every receiver entry point.
//!
//! `setup_transfer` activates input multiplex, reads the filter list, receives
//! the (possibly incremental) file list, sanitizes paths, and builds the
//! `PipelineSetup` used by `run_sync`, `run_pipelined`, and
//! `run_pipelined_incremental`. Filter-list wire parsing lives alongside it in
//! `parse_wire_filters_for_receiver`.

mod context;
mod wire_filters;

#[cfg(unix)]
mod sandbox;

#[cfg(test)]
mod tests;
