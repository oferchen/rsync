// upstream: clientserver.c - `daemon_main()` binds the TCP listener, enters
// the accept loop, and forks a child per connection. The thread-based model
// replaces fork with `std::thread::spawn` + `catch_unwind`.

include!("server_runtime/listener.rs");

include!("server_runtime/socket_options.rs");

include!("server_runtime/pid_file.rs");

include!("server_runtime/connection_counter.rs");

include!("server_runtime/workers.rs");

include!("server_runtime/reload.rs");

include!("server_runtime/connection.rs");

include!("server_runtime/connection_context.rs");

include!("server_runtime/accept_engine.rs");

include!("server_runtime/accept_loop.rs");

// QUIC/UDP listener binding (oc extension). Unix-only daemon; gated so default
// and non-quic builds are byte-identical. Bound alongside the TCP listeners in
// `serve_connections`; the accept()->session handoff lands under QUIC task #55.
#[cfg(all(unix, feature = "quic"))]
include!("server_runtime/quic_listener.rs");

#[cfg(test)]
#[path = "server_runtime/tests.rs"]
mod server_runtime_tests;
