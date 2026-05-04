#![cfg_attr(not(test), deny(unsafe_code))]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Transfer engine - delta pipeline, local-copy executor, and sparse I/O.
//!
//! # Purpose
//!
//! `engine` implements the core transfer primitives for the rsync
//! reimplementation. It sits between the high-level `core` orchestration facade
//! and the low-level `protocol`, `checksums`, `compress`, and `metadata` crates.
//! Both the CLI local-copy path and the remote sender/receiver/generator roles
//! in the `transfer` crate drive their file operations through this crate.
//!
//! # Capabilities
//!
//! ## Delta pipeline
//!
//! The [`delta`] module implements rsync's block-matching pipeline:
//! - [`SignatureLayout`] / [`calculate_signature_layout`] - upstream-compatible
//!   block-size heuristics (mirrors `rsync.c:read_batch_protocol()`).
//! - [`DeltaGenerator`] - produces [`DeltaToken`] streams (`LITERAL` / `COPY`)
//!   by matching rolling checksums against a [`DeltaSignatureIndex`].
//! - [`apply_delta`] - reconstructs destination data from a [`DeltaScript`].
//! - [`generate_delta`] / [`generate_file_signature`] - end-to-end helpers used
//!   by the sender and generator roles.
//!
//! ## Local-copy executor
//!
//! [`local_copy::LocalCopyPlan`] drives recursive, wire-compatible local
//! transfers (regular files, directories, symbolic links, FIFOs). Key features:
//! - Temp-file write + atomic rename (`--inplace` bypasses the rename).
//! - Sparse file support via [`SparseWriter`] / [`SparseReader`] / [`SparseDetector`]
//!   with 16-byte `u128` zero-run detection and a single seek-per-zero-run invariant.
//! - Deletion passes controlled by [`DeleteTiming`] (before/after transfer).
//! - Backup paths computed by [`compute_backup_path`].
//! - Reference-directory comparisons via [`ReferenceDirectory`].
//! - Filter-program execution via the `local_copy::filter_program` submodule.
//! - Directory merging via the `local_copy::dir_merge` submodule.
//!
//! ## Buffer reuse and vectored I/O
//!
//! `local_copy::buffer_pool` provides a `BufferPool` with RAII `PooledBuffer`
//! handles that return allocations to a shared pool on drop, eliminating
//! per-file heap churn on the hot transfer path.
//!
//! ## Fuzzy matching
//!
//! [`FuzzyMatcher`] / [`compute_similarity_score`] locate similar basis files
//! for the `--fuzzy` option, reducing literal-data transmission when an exact
//! basis is absent.
//!
//! ## Hardlink tracking
//!
//! [`HardlinkResolver`] / [`HardlinkTracker`] detect and re-create hardlink
//! groups across the destination tree, matching upstream rsync's `--hard-links`
//! semantics.
//!
//! ## Batch mode
//!
//! The re-exported `batch` module supports offline transfer workflows: write a
//! batch file with [`BatchWriter`], replay it later with [`BatchReader`] /
//! `batch::replay::replay`.
//!
//! ## Async I/O (optional)
//!
//! When compiled with the `async` feature, [`AsyncFileCopier`] and
//! [`AsyncBatchCopier`] provide tokio-based async file copy with progress
//! reporting via [`CopyProgress`].
//!
//! # Dependency position
//!
//! ```text
//! core
//!  └── engine
//!       ├── protocol    (wire framing, checksums, multiplex)
//!       ├── checksums   (rolling rsum, MD4/MD5/XXH3, SIMD)
//!       ├── signature   (signature layout + generation)
//!       ├── compress    (zlib/zstd/lz4 codecs)
//!       ├── metadata    (perms/uid/gid/mtime/xattrs/ACLs)
//!       ├── filters     (include/exclude rule evaluation)
//!       ├── bandwidth   (rate-limiting)
//!       ├── batch       (batch-mode file I/O)
//!       ├── matching    (fuzzy basis-file scoring)
//!       └── fast_io     (io_uring on Linux 5.6+, std fallback)
//! ```
//!
//! # Invariants
//!
//! - Plans derived from CLI operands are immutable after construction; callers
//!   may inspect planned operations before executing them.
//! - File contents are written before metadata is applied, matching upstream
//!   rsync's ordering.
//! - Errors preserve path, action, and exit-code context so `core` can surface
//!   canonical diagnostics without re-parsing strings.
//!
//! # Errors
//!
//! [`local_copy::LocalCopyError`] separates invalid-operand errors from I/O
//! failures. Each variant records the exit code upstream rsync would emit,
//! letting `core` produce identical diagnostics.
//!
//! [`EngineError`] is the top-level error type for operations that span
//! multiple subsystems (delta, walk, hardlinks).
//!
//! # Examples
//!
//! Construct a plan from CLI-style operands and execute it to copy a file:
//!
//! ```
//! use engine::local_copy::LocalCopyPlan;
//! use std::ffi::OsString;
//!
//! # let temp = tempfile::tempdir().unwrap();
//! # let source = temp.path().join("src.txt");
//! # std::fs::write(&source, b"data").unwrap();
//! # let destination = temp.path().join("dst.txt");
//! # std::fs::write(&destination, b"").unwrap();
//! let operands = vec![
//!     source.into_os_string(),
//!     destination.into_os_string(),
//! ];
//! let plan = LocalCopyPlan::from_operands(&operands).expect("plan succeeds");
//! plan.execute().expect("copy succeeds");
//! ```

#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
pub mod async_io;

pub mod concurrent_delta;
pub mod delta;
pub mod error;
pub mod hardlink;
pub mod local_copy;
pub mod walk;

#[doc(hidden)]
pub mod batch {
    //! Re-exports from the [`batch`] crate for backward compatibility.
    pub use batch::{
        BatchConfig, BatchError, BatchFlags, BatchHeader, BatchMode, BatchReader, BatchResult,
        BatchStats, BatchWriter, DeltaOp, FileEntry, ReplayResult,
    };

    /// Batch replay functions for applying recorded delta operations.
    pub mod replay {
        pub use batch::replay::{apply_delta_ops, replay};
    }

    /// Script generation for batch replay.
    pub mod script {
        pub use batch::script::{generate_script, generate_script_with_args};
    }
}

#[doc(hidden)]
pub mod fuzzy {
    //! Re-exports from the [`matching`] crate for backward compatibility.
    pub use matching::{
        FUZZY_LEVEL_1, FUZZY_LEVEL_2, FuzzyMatch, FuzzyMatcher, compute_similarity_score,
    };
}

#[doc(hidden)]
pub mod signature {
    //! Re-exports from the [`signature`] crate for backward compatibility.
    pub use signature::{
        FileSignature, SignatureAlgorithm, SignatureBlock, SignatureError, generate_file_signature,
    };
}

/// Batch mode types for offline transfer workflows.
pub use batch::{BatchConfig, BatchFlags, BatchHeader, BatchMode, BatchReader, BatchWriter};

/// Delta generation and signature layout for rsync block matching.
pub use delta::{
    DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, SignatureLayout,
    SignatureLayoutError, SignatureLayoutParams, apply_delta, calculate_signature_layout,
    generate_delta,
};

/// Concurrent delta pipeline work-item, result, and strategy types.
pub use concurrent_delta::{
    AdaptiveCapacityPolicy, DeltaResult, DeltaResultStatus, DeltaStrategy, DeltaTransferStrategy,
    DeltaWork, DeltaWorkKind, FileNdx, ReorderBuffer, ReorderStats, WholeFileStrategy,
};

/// Common error types for engine operations.
pub use error::{EngineError, EngineResult};

/// Fuzzy matching for finding similar basis files.
pub use fuzzy::{FUZZY_LEVEL_1, FUZZY_LEVEL_2, FuzzyMatch, FuzzyMatcher, compute_similarity_score};

/// Hardlink detection and resolution.
pub use hardlink::{HardlinkAction, HardlinkGroup, HardlinkKey, HardlinkResolver, HardlinkTracker};

/// Buffer pool for cross-crate I/O buffer reuse.
///
/// The pool uses a two-level cache (thread-local fast path + central Mutex) to
/// eliminate per-file allocation overhead on the hot transfer path. Buffers are
/// returned automatically via RAII guards on drop.
///
/// Use [`global_buffer_pool`] for the process-wide singleton, or create a
/// dedicated [`BufferPool`] for isolated subsystems.
pub use local_copy::{
    BorrowedBufferGuard, BufferAllocator, BufferGuard, BufferPool, BufferPoolStats,
    DefaultAllocator, GlobalBufferPoolConfig, ThroughputTracker, global_buffer_pool,
    init_global_buffer_pool,
};

/// Local filesystem copy operations.
pub use local_copy::{
    BuilderError, DeleteTiming, HardlinkApplyResult, HardlinkApplyTracker, LocalCopyArgumentError,
    LocalCopyError, LocalCopyErrorKind, LocalCopyOptions, LocalCopyOptionsBuilder, LocalCopyPlan,
    LocalCopySummary, ReferenceDirectory, ReferenceDirectoryKind, SkipCompressList,
    SkipCompressParseError, SparseDetector, SparseReader, SparseRegion, SparseWriter,
    compute_backup_path,
};

/// File signature generation for delta transfers.
pub use signature::{
    FileSignature, SignatureAlgorithm, SignatureBlock, SignatureError, generate_file_signature,
};

/// Directory traversal abstractions for file list generation.
pub use walk::{DirectoryWalker, FilteredWalker, WalkConfig, WalkEntry, WalkError, WalkdirWalker};

/// Async I/O operations (available with `async` feature).
#[cfg(feature = "async")]
pub use async_io::{AsyncBatchCopier, AsyncFileCopier, AsyncIoError, CopyProgress, CopyResult};
