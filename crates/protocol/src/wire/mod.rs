#![deny(unsafe_code)]
//! Wire protocol serialization for signatures and deltas.
//!
//! This module provides the serialization and deserialization logic for the
//! rsync protocol's data structures. The formats mirror upstream rsync 3.4.1
//! to ensure interoperability.
//!
//! For file list encoding/decoding, see the [`crate::flist`] module which
//! provides the full-featured implementation used in production.

pub mod compressed_token;
pub mod delta;
pub mod signature;

pub use self::compressed_token::{
    CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG,
    MAX_DATA_COUNT, TOKENRUN_LONG, TOKENRUN_REL, TOKEN_LONG, TOKEN_REL,
};
pub use self::delta::{
    // Upstream wire format
    CHUNK_SIZE,
    // Internal format
    DeltaOp,
    read_delta,
    read_int,
    read_token,
    write_delta,
    write_int,
    write_token_block_match,
    write_token_end,
    write_token_literal,
    write_token_stream,
    write_whole_file_delta,
};
pub use self::signature::{SignatureBlock, read_signature, write_signature};
