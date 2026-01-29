#![deny(unsafe_code)]

//! crates/signature/src/lib.rs
//!
//! File signature layout and generation for the Rust rsync implementation.
//!
//! This crate provides the core types and functions for computing rsync-compatible
//! file signatures. It combines:
//! - Block sizing heuristics from upstream rsync's `generator.c:sum_sizes_sqroot()`
//! - Signature generation using rolling and strong checksums
//!
//! # Overview
//!
//! The signature process involves two steps:
//! 1. Calculate the [`SignatureLayout`] based on file size, protocol version, and
//!    negotiated checksum parameters using [`calculate_signature_layout`]
//! 2. Generate the [`FileSignature`] by reading blocks and computing checksums
//!    using [`generate_file_signature`]
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

#![allow(clippy::module_name_repetitions)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod algorithm;
mod block;
mod file;
mod generation;
mod layout;

#[cfg(feature = "parallel")]
#[cfg_attr(docsrs, doc(cfg(feature = "parallel")))]
pub mod parallel;

pub mod async_gen;

pub use algorithm::SignatureAlgorithm;
pub use block::SignatureBlock;
pub use file::FileSignature;
pub use generation::{SignatureError, generate_file_signature};
pub use layout::{
    SignatureLayout, SignatureLayoutError, SignatureLayoutParams, calculate_signature_layout,
};
