#![deny(unsafe_code)]

//! crates/signature/src/lib.rs
//!
//! File signature layout and generation for the Rust rsync implementation.
//!
//! This crate provides the core types and functions for computing rsync-compatible
//! file signatures. It combines:
//! - Block sizing heuristics from upstream rsync's `generator.c:sum_sizes_sqroot()`
//! - Signature generation using rolling and strong checksums
//! - Standalone block size calculation matching rsync 3.4.1
//!
//! # Overview
//!
//! The signature process involves two steps:
//! 1. Calculate the [`SignatureLayout`] based on file size, protocol version, and
//!    negotiated checksum parameters using [`calculate_signature_layout`]
//! 2. Generate the [`FileSignature`] by reading blocks and computing checksums
//!    using [`generate_file_signature`]
//!
//! For more granular control, you can use the [`block_size`] module to calculate
//! block sizes and checksum lengths independently
//!
//! # Example
//!
//! ```
//! use signature::{
//!     SignatureLayoutParams, calculate_signature_layout,
//!     SignatureAlgorithm, generate_file_signature,
//! };
//! use protocol::ProtocolVersion;
//! use std::io::Cursor;
//! use std::num::NonZeroU8;
//!
//! // Step 1: Calculate layout
//! let params = SignatureLayoutParams::new(
//!     11,
//!     None,
//!     ProtocolVersion::NEWEST,
//!     NonZeroU8::new(16).unwrap(),
//! );
//! let layout = calculate_signature_layout(params).expect("layout");
//!
//! // Step 2: Generate signature
//! let input = Cursor::new(b"hello world".to_vec());
//! let signature = generate_file_signature(
//!     input,
//!     layout,
//!     SignatureAlgorithm::Md4,
//! ).expect("signature");
//!
//! assert_eq!(signature.blocks().len(), 1);
//! assert_eq!(signature.total_bytes(), 11);
//! ```
//!
//! # Block Size Calculation
//!
//! The [`block_size`] module provides standalone functions for calculating block
//! sizes and checksum lengths. This is useful when you need to determine parameters
//! before generating signatures:
//!
//! ```
//! use signature::{calculate_block_length, calculate_checksum_length, calculate_checksum_count};
//!
//! // Calculate optimal block size for a 10 MB file
//! let file_size = 10 * 1024 * 1024;
//! let protocol_version = 31;
//! let block_len = calculate_block_length(file_size, protocol_version, None);
//! assert_eq!(block_len, 3232); // sqrt-based scaling
//!
//! // Calculate checksum length
//! let checksum_len = calculate_checksum_length(file_size, block_len, protocol_version, 2);
//! assert!(checksum_len >= 2);
//!
//! // Calculate number of blocks
//! let block_count = calculate_checksum_count(file_size, block_len);
//! assert_eq!(block_count, 3245);
//! ```

#![allow(clippy::module_name_repetitions)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod algorithm;
mod block;
mod file;
mod generation;
mod layout;

/// Block size calculation algorithm matching upstream rsync 3.4.1.
///
/// Provides standalone functions for calculating block sizes and checksum lengths
/// used in rsync's delta transfer algorithm.
pub mod block_size;

/// Pipelined signature generation with double-buffered I/O.
pub mod pipelined_gen;

pub mod parallel;

pub mod async_gen;

pub use algorithm::SignatureAlgorithm;
pub use block::SignatureBlock;
pub use block_size::{
    calculate_block_length, calculate_checksum_count, calculate_checksum_length,
    DEFAULT_BLOCK_SIZE, MAX_BLOCK_SIZE_OLD, MAX_BLOCK_SIZE_V30, MIN_BLOCK_SIZE,
};
pub use file::FileSignature;
pub use generation::{SignatureError, generate_file_signature};
pub use layout::{
    SignatureLayout, SignatureLayoutError, SignatureLayoutParams, calculate_signature_layout,
};
pub use pipelined_gen::{PipelinedSignatureConfig, generate_signature_pipelined};
