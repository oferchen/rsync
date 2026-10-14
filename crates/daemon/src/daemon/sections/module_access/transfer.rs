// Transfer execution - stream setup, handshake building, and transfer lifecycle.
//
// Handles the final phase of a module request: validating the module path,
// applying chroot and privilege restrictions, spawning the name converter,
// running pre/post-xfer exec hooks, and invoking the Rust transfer engine.
//
// upstream: clientserver.c:rsync_module() - after authentication it chroots to
// the module path (1040-1057), changes into it (1059), drops privileges with
// `setgid()`/`setuid()` (1096-1135), and only then reads the client arguments
// (1152-1168) and enters the transfer pipeline via `start_server()` (1304).
//
// This file is `include!`d into the `crate::daemon` scope (see
// `module_access.rs`), so the sub-parts below are textually included rather
// than declared as `mod`s. They share the imports `daemon.rs` provides and
// remain in one flat module scope, keeping every function visible to the
// sibling `request.rs` / `tests.rs` callers exactly as before.

include!("transfer/sandbox.rs");

include!("transfer/io_progress.rs");

include!("transfer/draining_reader.rs");

include!("transfer/graceful_close.rs");

include!("transfer/streams.rs");

include!("transfer/orchestration.rs");
