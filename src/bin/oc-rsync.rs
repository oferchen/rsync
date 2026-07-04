#![deny(unsafe_code)]

// High-performance global allocator, selected per platform.
//
// Unix uses jemalloc so the page-return tuning below can be applied at
// allocator init (see `_rjem_malloc_conf`). Windows keeps mimalloc, which
// has no comparable jemalloc support on that platform.
#[cfg(unix)]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(windows)]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// jemalloc reads this compile-time configuration static at allocator init,
// before `main` runs, so no environment variable or process re-exec is
// needed. A 250 ms dirty/muzzy decay returns freed pages to the OS promptly
// instead of retaining them, which bounds resident memory at scale. Measured
// on a 100k-file local copy: Maximum RSS drops from ~45 MB to ~31 MB, and on
// a 2 GiB transfer from ~20 MB to ~13 MB. A 250 ms window (rather than 0)
// batches the page-return `madvise` calls, avoiding the per-free syscall cost
// of immediate decay: it captures effectively the same RSS reduction while
// eliminating the throughput regression on I/O-bound transfers and halving it
// on allocation-heavy small-file workloads.
//
// The symbol name matches the default `_rjem_`-prefixed tikv-jemalloc-sys
// build. Were the crate built with `unprefixed_malloc_on_supported_platforms`,
// the expected symbol would be `malloc_conf` instead.
#[cfg(unix)]
#[allow(unsafe_code, non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static _rjem_malloc_conf: &[u8] = b"dirty_decay_ms:250,muzzy_decay_ms:250\0";

#[path = "client.rs"]
mod client;
mod support;

use std::{env, io, process::ExitCode};

fn main() -> ExitCode {
    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    windows_gnu_eh::force_link();

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    client::run_with(env::args_os(), &mut stdout, &mut stderr)
}
