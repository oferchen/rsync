#![deny(unsafe_code)]
//! Delta token wire format for file reconstruction.
//!
//! This module implements serialization for delta operations used to reconstruct
//! files from a basis file. Delta streams consist of literal data writes and
//! copy operations that reference blocks in the basis file.
//!
//! ## Wire Format (Upstream rsync compatibility)
//!
//! Upstream rsync uses a simple token format in `token.c:simple_send_token()`:
//!
//! - **Literal data**: `write_int(length)` (positive i32 LE) followed by raw bytes
//!   - Large literals are chunked into CHUNK_SIZE (32KB) pieces
//! - **Block match**: `write_int(-(token+1))` where token is the block index
//!   - Example: block 0 = -1, block 1 = -2, etc.
//! - **End marker**: `write_int(-1)` when sum_count=0 (whole-file transfer)
//!
//! References:
//! - `token.c:simple_send_token()` line ~305
//! - `io.c:write_int()` line ~2082
//!
//! ## Submodules
//!
//! - `int_encoding` - Fundamental 4-byte LE integer read/write primitives
//! - `token` - Upstream token-based wire format (literals, block matches, end markers)
//! - `internal` - Internal opcode-based delta format for backward compatibility
//! - `types` - Core types and constants (`DeltaOp`, `CHUNK_SIZE`)

mod int_encoding;
mod internal;
mod token;
mod types;

#[cfg(test)]
mod tests;

pub use self::int_encoding::{read_int, write_int};
pub use self::internal::{read_delta, read_delta_op, write_delta, write_delta_op};
pub use self::token::{
    read_token, write_token_block_match, write_token_end, write_token_literal, write_token_stream,
    write_whole_file_delta,
};
pub use self::types::{CHUNK_SIZE, DeltaOp};
