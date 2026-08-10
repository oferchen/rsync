//! `--debug=NSTR` producer surface for the algorithm-negotiation strings.
//!
//! Wraps the upstream `compat.c` / `checksum.c` `DEBUG_GTE(NSTR, N)`
//! emission sites in a single helper module so callers across the
//! workspace share one byte-for-byte definition of each emission shape.
//!
//! See `trace` for the producer helpers and upstream-reference notes.

pub mod trace;

pub use trace::{
    CLVL_NOT_SPECIFIED, NstrCategory, NstrSide, trace_checksum_summary, trace_compress_summary,
    trace_daemon_auth_negotiated, trace_daemon_greeting_auth_list, trace_recv_list,
    trace_send_list,
};
