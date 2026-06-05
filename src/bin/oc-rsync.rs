#![deny(unsafe_code)]

use mimalloc::MiMalloc;

/// High-performance memory allocator for improved allocation throughput.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[path = "client.rs"]
mod client;
mod support;

use std::{env, io, process::ExitCode};

/// Set mimalloc environment variable defaults before the allocator reads them.
///
/// Mimalloc's default 1 GiB virtual arena reservation and deferred purge cause
/// ~16 MiB RSS overhead from committed-but-purged pages (RSS-2 profiling root
/// cause #5). Reducing the arena to 128 MiB and setting immediate purge closes
/// most of that gap while preserving allocation throughput.
///
/// Only sets defaults - user-supplied `MIMALLOC_*` env vars take precedence.
///
/// # Safety
///
/// Called at the start of `main()` before any threads are spawned, so the
/// single-threaded precondition for `env::set_var` is satisfied.
#[allow(unsafe_code)]
fn configure_mimalloc_defaults() {
    if env::var_os("MIMALLOC_ARENA_RESERVE").is_none() {
        // Reduce from default 1 GiB to 128 MiB to cut virtual arena overhead.
        unsafe { env::set_var("MIMALLOC_ARENA_RESERVE", "128m") };
    }
    if env::var_os("MIMALLOC_PURGE_DELAY").is_none() {
        // Decommit freed pages immediately instead of deferring (default 10ms).
        unsafe { env::set_var("MIMALLOC_PURGE_DELAY", "0") };
    }
}

fn main() -> ExitCode {
    configure_mimalloc_defaults();

    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    windows_gnu_eh::force_link();

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    client::run_with(env::args_os(), &mut stdout, &mut stderr)
}
