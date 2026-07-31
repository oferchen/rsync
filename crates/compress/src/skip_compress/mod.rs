//! Default skip-compress suffix list.
//!
//! This module mirrors upstream rsync's `--skip-compress` default suffix set,
//! which lists file extensions that typically don't benefit from compression
//! during transfer. The per-file gate that consumes this list lives in the
//! engine (`local_copy::options::should_skip_compress`), matching upstream's
//! last-suffix comparison in `token.c:init_set_compression()`.

mod defaults;

pub use defaults::DEFAULT_SKIP_COMPRESS_SUFFIXES;
