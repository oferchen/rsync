//! Default skip-compress suffix list.
//!
//! This module mirrors upstream rsync's `--skip-compress` default suffix set,
//! which lists file extensions that typically don't benefit from compression
//! during transfer. The per-file gate that consumes this list lives in the
//! engine (`local_copy::options::should_skip_compress`) and mirrors upstream's
//! last-suffix comparison in token.c:232 `set_compression()`.
//!
//! Deviation from upstream: 3.5.1 compiles that per-file comparison out
//! (`#if 0`, no codec allows a mid-stream level change), so upstream never
//! skips compression per file; oc's local-copy gate does.

mod defaults;

pub use defaults::DEFAULT_SKIP_COMPRESS_SUFFIXES;
