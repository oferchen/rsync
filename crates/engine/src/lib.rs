#![cfg_attr(not(feature = "acl"), deny(unsafe_code))]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `engine` hosts transfer-oriented building blocks that power the Rust
//! rsync implementation. The crate currently focuses on deterministic local
//! filesystem copies so higher layers can provide observable behaviour while the
//! full delta-transfer pipeline is under construction.
//!
//! # Design
//!
//! Functionality is decomposed into focused modules. The [`local_copy`] module
//! exposes [`local_copy::LocalCopyPlan`] which performs
//! recursive copies of regular files, directories, symbolic links, and FIFOs
//! while preserving permissions and timestamps through the [`metadata`]
//! helpers. The [`delta`] module mirrors upstream rsync's signature layout
//! heuristics so the delta-transfer pipeline can reuse byte-identical block
//! sizing when it lands, while [`signature`] turns those layouts into
//! rsync-compatible rolling/strong checksum streams ready for transmission. The
//! design keeps path parsing and copying logic in the engine layer so both the
//! CLI and daemon facades can drive local transfers through a single interface.
//!
//! # Invariants
//!
//! - Plans derived from CLI-style operands never modify the source list after
//!   construction, allowing callers to inspect the planned operations before
//!   execution.
//! - Copy operations apply metadata only after file contents are written,
//!   matching upstream rsync's ordering.
//! - Errors preserve enough context (path, action, exit code) for higher layers
//!   to render canonical diagnostics without re-parsing strings.
//!
//! # Errors
//!
//! [`local_copy::LocalCopyError`] classifies invalid operands separately from
//! I/O failures. Each error records the exit code that upstream rsync would have
//! used, allowing the `core` crate to surface identical diagnostics.
//!
//! # Examples
//!
//! Construct a plan from CLI-style operands and execute it to copy a file:
//!
//! ```
//! use engine::local_copy::LocalCopyPlan;
//! use std::ffi::OsString;
//!
//! let operands = vec![OsString::from("src.txt"), OsString::from("dst.txt")];
//! let plan = LocalCopyPlan::from_operands(&operands).expect("plan succeeds");
//! // Plan execution copies the file once the surrounding code has created it.
//! # let temp = tempfile::tempdir().unwrap();
//! # let source = temp.path().join("src.txt");
//! # std::fs::write(&source, b"data").unwrap();
//! # let destination = temp.path().join("dst.txt");
//! # std::fs::write(&destination, b"").unwrap();
//! # let operands = vec![
//! #     source.clone().into_os_string(),
//! #     destination.clone().into_os_string(),
//! # ];
//! # let plan = LocalCopyPlan::from_operands(&operands).unwrap();
//! plan.execute().expect("copy succeeds");
//! ```
//!
//! # See also
//!
//! - `core::client` integrates the plan builder to power the `rsync`
//!   binary's local copy mode.
//! - [`delta`] exposes block-size and checksum heuristics that will be wired into
//!   the delta-transfer engine.
//! - [`signature`] generates rolling and strong checksum pairs from those
//!   layouts so forthcoming protocol layers can transmit signatures without
//!   shelling out to upstream rsync.

#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
pub mod async_io;

pub mod delta;
pub mod error;
pub mod local_copy;
pub mod walk;

// Re-export batch types from the batch crate for backward compatibility.
// The batch crate is the source of truth for these types.
#[doc(hidden)]
pub mod batch {
    //! Re-exports from the [`batch`] crate for backward compatibility.
    pub use batch::{
        BatchConfig, BatchError, BatchFlags, BatchHeader, BatchMode, BatchReader, BatchResult,
        BatchWriter, DeltaOp, FileEntry,
    };

    /// Script generation for batch replay.
    pub mod script {
        pub use batch::script::{generate_script, generate_script_with_args};
    }
}

// Re-export fuzzy matching types from the matching crate for backward compatibility.
// The matching crate is the source of truth for these types.
#[doc(hidden)]
pub mod fuzzy {
    //! Re-exports from the [`matching`] crate for backward compatibility.
    pub use matching::{FuzzyMatch, FuzzyMatcher, compute_similarity_score};
}

// Re-export signature types from the dedicated signature crate for backward compatibility.
// The signature crate is the source of truth for these types.
#[doc(hidden)]
pub mod signature {
    //! Re-exports from the [`signature`] crate for backward compatibility.
    pub use signature::{
        FileSignature, SignatureAlgorithm, SignatureBlock, SignatureError, generate_file_signature,
    };
}

pub use batch::{BatchConfig, BatchFlags, BatchHeader, BatchMode, BatchReader, BatchWriter};
pub use delta::{
    DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, SignatureLayout,
    SignatureLayoutError, SignatureLayoutParams, apply_delta, calculate_signature_layout,
    generate_delta,
};
pub use error::{EngineError, EngineResult};
pub use fuzzy::{FuzzyMatch, FuzzyMatcher, compute_similarity_score};
pub use local_copy::{
    DeleteTiming, LocalCopyArgumentError, LocalCopyError, LocalCopyErrorKind, LocalCopyOptions,
    LocalCopyPlan, LocalCopySummary, ReferenceDirectory, ReferenceDirectoryKind, SkipCompressList,
    SkipCompressParseError,
};
pub use signature::{
    FileSignature, SignatureAlgorithm, SignatureBlock, SignatureError, generate_file_signature,
};
pub use walk::{DirectoryWalker, FilteredWalker, WalkConfig, WalkEntry, WalkError, WalkdirWalker};

#[cfg(feature = "async")]
pub use async_io::{
    AsyncBatchCopier, AsyncFileCopier, AsyncFileReader, AsyncFileWriter, AsyncIoError,
    ChecksumAlgorithm, CopyProgress, CopyResult, compute_file_checksum,
};
