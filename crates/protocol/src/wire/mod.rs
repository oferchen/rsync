#![deny(unsafe_code)]
//! Wire protocol serialization for signatures, deltas, and file entries.
//!
//! This module provides the serialization and deserialization logic for the
//! rsync protocol's data structures. The formats mirror upstream rsync 3.4.1
//! to ensure interoperability.
//!
//! # Submodules
//!
//! - [`file_entry`] - Low-level file entry wire format encoding functions
//! - [`signature`] - Signature block encoding for delta generation
//! - [`delta`] - Delta token encoding for file reconstruction
//! - [`compressed_token`] - Compressed token stream handling
//!
//! For high-level file list encoding/decoding, see the [`crate::flist`] module
//! which provides [`crate::flist::FileListWriter`] and [`crate::flist::FileListReader`].

pub mod compressed_token;
pub mod delta;
pub mod file_entry;
pub mod signature;

pub use self::compressed_token::{
    CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG,
    MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
};
pub use self::delta::{
    // Upstream wire format
    CHUNK_SIZE,
    // Internal format
    DeltaOp,
    read_delta,
    read_delta_op,
    read_int,
    read_token,
    write_delta,
    write_delta_op,
    write_int,
    write_token_block_match,
    write_token_end,
    write_token_literal,
    write_token_stream,
    write_whole_file_delta,
};
pub use self::signature::{SignatureBlock, read_signature, write_signature};

// File entry wire format encoding
pub use self::file_entry::{
    // Flag encoding
    encode_end_marker, encode_flags,
    // Name encoding
    calculate_name_prefix_len, encode_name,
    // Metadata encoding
    encode_atime, encode_checksum, encode_crtime, encode_gid, encode_mode, encode_mtime,
    encode_mtime_nsec, encode_owner_name, encode_rdev, encode_size, encode_symlink_target,
    encode_uid,
    // Hardlink encoding
    encode_hardlink_dev_ino, encode_hardlink_idx,
    // Flag calculation helpers
    calculate_basic_flags, calculate_device_flags, calculate_hardlink_flags, calculate_time_flags,
};
