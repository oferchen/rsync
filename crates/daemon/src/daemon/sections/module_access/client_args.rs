// Client argument reading and server configuration building.
//
// After the daemon sends `@RSYNCD: OK`, the client transmits its command-line
// arguments (the same arguments that `server_options()` would produce for an
// SSH-mode server invocation). The daemon parses these to configure the
// transfer engine with the correct flags, paths, and options.
//
// upstream: io.c:1308 - `read_args()` reads null/newline-terminated arguments.
// options.c:2755-2998 - `server_options()` emits the long-form options.
// clientserver.c:1073-1087 - two-phase secluded-args reading.
//
// This file is `include!`d into the `crate::daemon` scope (see
// `module_access.rs`), so the sub-parts below are textually included rather
// than declared as `mod`s. They share the imports `daemon.rs` provides and
// remain in one flat module scope, keeping every function visible to the
// sibling `transfer.rs` / `request.rs` callers exactly as before.

include!("client_args/arg_reading.rs");

include!("client_args/path_resolution.rs");

include!("client_args/server_config.rs");

include!("client_args/long_form_args.rs");

include!("client_args/module_directives.rs");

include!("client_args/tests.rs");
