// Server runtime - decomposed into focused submodules following SRP.
// Each submodule handles a single responsibility within the daemon accept loop.

include!("server_runtime/listener.rs");

include!("server_runtime/socket_options.rs");

include!("server_runtime/pid_file.rs");

include!("server_runtime/connection_counter.rs");

include!("server_runtime/workers.rs");

include!("server_runtime/accept_loop.rs");

#[cfg(test)]
#[path = "server_runtime/tests.rs"]
mod server_runtime_tests;
