//! Strong checksum implementations backed by well-known hash algorithms.
//!
//! Upstream rsync negotiates the strong checksum algorithm based on the protocol
//! version and compile-time feature set. This module exposes streaming wrappers
//! for MD4, MD5, and XXH64 so higher layers can compose the desired strategy
//! without reimplementing the hashing primitives.

mod md4;
mod md5;
mod xxhash;

pub use md4::Md4;
pub use md5::Md5;
pub use xxhash::Xxh64;
