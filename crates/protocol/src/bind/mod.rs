//! `--debug=BIND` producer emissions for socket bind diagnostics.
//!
//! Hosts the trace helpers that mirror upstream rsync 3.4.4's
//! `DEBUG_GTE(BIND, 1)` diagnostics emitted from `socket.c::open_socket_in`
//! while iterating per-address-family during daemon listener setup.
//!
//! # Upstream Reference
//!
//! - `socket.c:432-438` - `"socket(%d,%d,%d) failed: %s\n"` accumulated when
//!   `socket(2)` returns `-1` for one of the address-family candidates.
//! - `socket.c:461-470` - `"bind() failed: %s (address-family %d)\n"`
//!   accumulated when `bind(2)` returns `-1` for the chosen socket.
//! - `socket.c:479-486` - flush loop: every accumulated message fires when
//!   either no listener bound (`!i`) or `--debug=BIND` is at level 1 or
//!   higher.
//! - `options.c:292` - `DEBUG_WORD(BIND, W_CLI, ...)` flag table entry,
//!   capping useful emissions at level 1.

pub mod trace;

pub use trace::{trace_bind_failure, trace_socket_failure};
