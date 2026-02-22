#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! # Overview
//!
//! `daemon` implements the rsync daemon mode (`--daemon`), providing a TCP
//! listener that accepts rsync protocol connections, performs `@RSYNCD:`
//! greeting negotiation with protocol `32`, serves `#list` module listings,
//! authenticates `auth users` via challenge/response backed by a secrets
//! file (with strict-modes permission enforcement), and executes file
//! transfers natively using the Rust transfer engine.
//!
//! Both legacy ASCII and modern binary protocol negotiation (protocols ≥ 30)
//! are supported. The daemon handles per-module access control, chroot,
//! uid/gid mapping, and configuration via `oc-rsyncd.conf`.
//!
//! # Design
//!
//! - [`run`] mirrors upstream `rsyncd` by accepting argument iterators together
//!   with writable handles for standard output and error streams.
//! - [`DaemonConfig`] stores the caller-provided daemon arguments. A
//!   [`DaemonConfigBuilder`] exposes an API that higher layers will expand once
//!   full daemon support lands.
//! - The runtime honours the branded `OC_RSYNC_CONFIG` and
//!   `OC_RSYNC_SECRETS` environment variables and falls back to the legacy
//!   `RSYNCD_CONFIG`/`RSYNCD_SECRETS` overrides when the branded values are
//!   unset. When no explicit configuration path is provided via CLI or
//!   environment variables, the daemon attempts to load
//!   `/etc/oc-rsyncd/oc-rsyncd.conf` so packaged defaults align with production
//!   deployments. If that path is absent the daemon also checks the legacy
//!   `/etc/rsyncd.conf` so existing installations continue to work during the
//!   transition to the prefixed configuration layout.
//! - [`run_daemon`] parses command-line arguments, binds a TCP listener, and
//!   serves one or more connections. It recognises both the legacy ASCII
//!   prologue and the binary negotiation used by modern clients, ensuring
//!   graceful diagnostics regardless of the handshake style. Requests for
//!   `#list` reuse the configured module table, while module transfers continue
//!   to emit availability diagnostics until the full engine lands.
//! - Authentication mirrors upstream rsync: the daemon issues a base64-encoded
//!   challenge, verifies the client's response against the configured secrets
//!   file using MD5, and only then reports that transfers are unavailable while
//!   the data path is under construction.
//! - A dedicated help renderer returns a deterministic description of the limited
//!   daemon capabilities available today, keeping the help text aligned with actual
//!   behaviour until the parity help renderer is implemented.
//!
//! # Process Model
//!
//! Upstream rsync forks a child process per connection (`main.c`), so a crash
//! in one transfer only kills that child while the parent continues accepting
//! connections. oc-rsync uses OS threads (sync mode) or tokio tasks (async
//! mode) instead, sharing one process across all connections.
//!
//! To match upstream's crash-isolation guarantee, every session handler is
//! wrapped in `std::panic::catch_unwind`.  A panic in one connection is
//! caught, logged to the daemon log file, and the thread exits cleanly.  The
//! daemon continues serving all other connections.  A second defense layer in
//! `join_worker` catches any panics that escape `catch_unwind` via
//! `JoinHandle::join`.
//!
//! The thread model was chosen over `fork` for cross-platform portability
//! (Windows has no `fork`), lower per-connection overhead, and efficient
//! shared-state access via `Arc`.  Rust's ownership model and
//! `#![deny(unsafe_code)]` on this crate eliminate the memory-corruption risks
//! that make fork's address-space isolation valuable in C.
//!
//! See `docs/DAEMON_PROCESS_MODEL.md` for a full comparison including
//! operational recommendations.
//!
//! # Invariants
//!
//! - Diagnostics are routed through [`core::message`] so trailers and
//!   source locations follow workspace conventions.
//! - `run` never panics. I/O failures propagate as exit code `1` with the
//!   original error rendered verbatim.
//! - [`DaemonError::exit_code`] always matches the exit code embedded within the
//!   associated [`core::message::Message`].
//! - `run_daemon` configures read and write timeouts on accepted sockets so
//!   handshake deadlocks are avoided, mirroring upstream rsync's timeout
//!   handling expectations.
//!
//! # Errors
//!
//! Parsing failures surface as exit code `1` and emit the `clap`-generated
//! diagnostic. Transfer attempts report that daemon functionality is currently
//! unavailable, also using exit code `1`.
//!
//! # Examples
//!
//! Render the `--version` banner into an in-memory buffer.
//!
//! ```
//! use daemon::run;
//!
//! let mut stdout = Vec::new();
//! let mut stderr = Vec::new();
//! let status = run(
//!     [
//!         core::branding::daemon_program_name(),
//!         "--version",
//!     ],
//!     &mut stdout,
//!     &mut stderr,
//! );
//!
//! assert_eq!(status, 0);
//! assert!(stderr.is_empty());
//! assert!(!stdout.is_empty());
//! ```
//!
//! Launching the daemon binds a TCP listener (defaulting to `0.0.0.0:873`),
//! accepts a legacy connection, and responds with an explanatory error.
//!
//! ```
//! use daemon::{run_daemon, DaemonConfig};
//! use std::io::{BufRead, BufReader, Write};
//! use std::net::{TcpListener, TcpStream};
//! use std::thread;
//! use std::time::Duration;
//!
//! # fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! # unsafe {
//! #     std::env::set_var("OC_RSYNC_DAEMON_FALLBACK", "0");
//! #     std::env::set_var("OC_RSYNC_FALLBACK", "0");
//! # }
//!
//! let listener = TcpListener::bind("127.0.0.1:0")?;
//! let port = listener.local_addr()?.port();
//! drop(listener);
//!
//! let config = DaemonConfig::builder()
//!     .disable_default_paths()
//!     .arguments(["--port", &port.to_string(), "--once"])
//!     .build();
//!
//! let handle = thread::spawn(move || run_daemon(config));
//!
//! let mut stream = loop {
//!     match TcpStream::connect(("127.0.0.1", port)) {
//!         Ok(stream) => break stream,
//!         Err(error) => {
//!             if error.kind() != std::io::ErrorKind::ConnectionRefused {
//!                 return Err(Box::new(error));
//!             }
//!         }
//!     }
//!     thread::sleep(Duration::from_millis(20));
//! };
//! let mut reader = BufReader::new(stream.try_clone()?);
//! let mut line = String::new();
//! reader.read_line(&mut line)?;
//! assert_eq!(line, "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n");
//! stream.write_all(b"@RSYNCD: 32.0\n")?;
//! stream.flush()?;
//! // Send a non-existent module name
//! stream.write_all(b"module\n")?;
//! stream.flush()?;
//! // For unknown modules, daemon sends @ERROR directly (no OK for unknown modules)
//! line.clear();
//! reader.read_line(&mut line)?;
//! assert!(line.starts_with("@ERROR:"));
//! line.clear();
//! reader.read_line(&mut line)?;
//! assert_eq!(line, "@RSYNCD: EXIT\n");
//!
//! handle.join().expect("thread").expect("daemon run succeeds");
//! # Ok(())
//! # }
//! # demo().unwrap();
//! ```

pub mod auth;
mod cli;
mod config;
mod daemon;
mod error;
pub mod rsyncd_config;
mod systemd;

#[cfg(test)]
mod test_env;

#[cfg(test)]
mod tests;

pub use cli::{exit_code_from, run};
pub use config::{DaemonConfig, DaemonConfigBuilder};
pub use daemon::run_daemon;
pub use error::DaemonError;
