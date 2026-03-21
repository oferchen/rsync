//! Checksum pipelining with double-buffering for overlapping computation with I/O.
//!
//! This module provides a dual-path checksum computation system that uses runtime
//! selection between pipelined and sequential modes based on workload characteristics.
//! Both code paths are always compiled to ensure consistent behavior and simplify testing.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                   Dual-Path Checksum Pipeline                            │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                           │
//! │  Sequential Path (< PIPELINE_THRESHOLD files):                           │
//! │  ┌─────────┐ ┌─────────┐ ┌─────────┐                                    │
//! │  │ Read A  │ │ Read B  │ │ Read C  │                                    │
//! │  └────┬────┘ └────┬────┘ └────┬────┘                                    │
//! │       │           │           │                                          │
//! │       ▼           ▼           ▼                                          │
//! │  ┌─────────┐ ┌─────────┐ ┌─────────┐                                    │
//! │  │ Hash A  │ │ Hash B  │ │ Hash C  │                                    │
//! │  └─────────┘ └─────────┘ └─────────┘                                    │
//! │                                                                           │
//! │  Pipelined Path (>= PIPELINE_THRESHOLD files):                           │
//! │  ┌─────────┐ ┌─────────┐ ┌─────────┐                                    │
//! │  │ Read A  │ │ Read B  │ │ Read C  │    (I/O Thread)                    │
//! │  └────┬────┘ └─────────┘ └─────────┘                                    │
//! │       │           ▲           ▲                                          │
//! │       │           │ Buffer    │ Buffer                                   │
//! │       │           │ swap      │ swap                                     │
//! │       ▼           │           │                                          │
//! │  ┌─────────┐ ┌────┴────┐ ┌───┴─────┐                                    │
//! │  │ Hash A  │ │ Hash B  │ │ Hash C  │    (Compute Thread)                │
//! │  └─────────┘ └─────────┘ └─────────┘                                    │
//! │                                                                           │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Double-Buffering
//!
//! The pipelined path uses two buffers to overlap I/O and computation:
//! - While computing checksum of buffer A, read next chunk into buffer B
//! - Swap buffers on completion, enabling continuous processing
//! - No crossbeam dependency - uses `std::sync::mpsc` channels
//!
//! # Performance Characteristics
//!
//! **Sequential Path:**
//! - Lower overhead for small workloads
//! - Predictable memory usage
//! - No thread synchronization costs
//!
//! **Pipelined Path:**
//! - 20-50% throughput improvement for I/O-bound workloads
//! - Benefits maximized with balanced I/O and compute times
//! - Best for >= 4 files (`PIPELINE_THRESHOLD`)
//!
//! # Example
//!
//! ```rust
//! use checksums::pipeline::{PipelinedChecksum, ChecksumInput};
//! use checksums::strong::Md5;
//! use std::io::Cursor;
//!
//! // Create input specifications
//! let inputs = vec![
//!     ChecksumInput::new(Cursor::new(vec![0u8; 1024]), 1024),
//!     ChecksumInput::new(Cursor::new(vec![1u8; 2048]), 2048),
//!     ChecksumInput::new(Cursor::new(vec![2u8; 512]), 512),
//! ];
//!
//! // Build pipelined checksum processor
//! let processor = PipelinedChecksum::builder()
//!     .buffer_size(4096)
//!     .build();
//!
//! // Process with automatic path selection
//! let results = processor.compute::<Md5, _>(inputs).unwrap();
//! assert_eq!(results.len(), 3);
//! ```

mod pipelined;
mod processor;
mod sequential;
mod types;

pub use pipelined::pipelined_checksum;
pub use processor::{PipelinedChecksum, PipelinedChecksumBuilder};
pub use sequential::sequential_checksum;
pub use types::{ChecksumInput, ChecksumResult, PIPELINE_THRESHOLD, PipelineConfig};

#[cfg(test)]
mod tests;
