//! `--debug=CMD` producer emissions for command and option construction.
//!
//! Hosts the trace helpers that mirror upstream rsync 3.4.4's `DEBUG_GTE(CMD, N)`
//! diagnostics emitted from `pipe.c`, `rsync.c`, `clientserver.c`, and `main.c`
//! during remote command construction, secluded-args transmission, and daemon
//! argument forwarding.
//!
//! # Upstream Reference
//!
//! - `pipe.c:54` (level 1) - `print_child_argv("opening connection using:", command)`
//!   in `piped_child()` before spawning the remote shell.
//! - `clientserver.c:348` (level 1) - `print_child_argv("sending daemon args:", sargs)`
//!   immediately before writing the daemon argument list to the socket.
//! - `rsync.c:296` (level 1) - `print_child_argv("protected args:", args + i + 1)`
//!   in `send_protected_args()` before the per-arg iconv loop.
//! - `main.c:620` (level 2) - per-argument `cmd[%d]=%s` enumeration inside
//!   `do_cmd()` once the final remote argv has been assembled.

pub mod trace;

pub use trace::{
    print_child_argv, trace_cmd_argv, trace_opening_connection, trace_protected_args,
    trace_sending_daemon_args,
};
