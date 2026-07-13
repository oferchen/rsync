// Transfer execution - stream setup, handshake building, and transfer lifecycle.
//
// Handles the final phase of a module request: validating the module path,
// applying chroot and privilege restrictions, spawning the name converter,
// running pre/post-xfer exec hooks, and invoking the Rust transfer engine.
//
// upstream: `clientserver.c` - after `rsync_module()` completes authentication
// and argument parsing, it calls `chdir(lp_path())`, `chroot(".")`,
// `setgid()`/`setuid()`, and then enters the transfer pipeline.
//
// This file is `include!`d into the `crate::daemon` scope (see
// `module_access.rs`), so the sub-parts below are textually included rather
// than declared as `mod`s. They share the imports `daemon.rs` provides and
// remain in one flat module scope, keeping every function visible to the
// sibling `request.rs` / `tests.rs` callers exactly as before.

include!("transfer/sandbox.rs");

include!("transfer/draining_reader.rs");

include!("transfer/graceful_close.rs");

include!("transfer/streams.rs");

include!("transfer/orchestration.rs");
