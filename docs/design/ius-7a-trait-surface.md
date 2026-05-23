# IUS-7.a - `IoUringBackend` trait surface

Date: 2026-05-23
Scope: design-only specification of a `fast_io::io_uring::IoUringBackend`
trait that shrinks the ~73 KB of mechanical duplication in
`crates/fast_io/src/io_uring_stub/` to a ~50 LoC stub by routing every
public operation through one trait with a real Linux impl and a tiny
non-Linux impl.
Status: **SPEC DRAFT** - no source changes in this PR; informs IUS-7.b
(zero-cost guarantee) and IUS-8.{a,b,c} (define / Linux impl / replace
stub crate).
Predecessor: `project_io_uring_stub_size.md` (memory note - trait
abstraction would shrink the stub).
Related: `docs/design/io-strategy-trait.md` (#1765 disposition - the
existing `IoBackend` trait is information-only by design; this spec
extends to *operations* without re-introducing the per-call vtable
penalty that #1765 deferred).

## 0. Why now

The non-Linux stub at `crates/fast_io/src/io_uring_stub/` is 21 source
files totalling 2,422 LoC (excluding the 409 LoC of stub-only tests),
laid out one-for-one against `crates/fast_io/src/io_uring/`. Every
public function, type, and constant is duplicated. Three failure modes
follow from the duplication:

1. **Drift hazard.** Every Linux-side signature change requires a
   matching stub edit; CI catches the build break but not semantic
   skews (different defaults, different error kinds).
2. **Review noise.** A 10-line Linux change typically forces a
   10-line stub change; the stub diff dwarfs the substantive diff in
   most reviews.
3. **Surface drift between platforms.** New Linux entry points
   sometimes ship without stubs (caught by `cargo check
   --target=x86_64-pc-windows-msvc`), or stubs drift to slightly
   different signatures over time.

A trait abstraction collapses the per-platform divergence to one impl
block per platform. The Linux impl forwards directly; the non-Linux
impl returns `IoUringError::Unsupported` once per method. Net effect:
the stub crate becomes one trait impl plus the marker type, instead of
21 mirror files.

## 1. Trait definition

The trait lives in `fast_io::io_uring::backend` (new submodule) and is
re-exported as `fast_io::io_uring::IoUringBackend`. Methods are grouped
by concern - availability, ring lifecycle, opcode dispatch, buffer
registration, probes - so impls can be read top-to-bottom.

```rust
use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::raw::c_int;
use std::path::Path;
use std::time::Duration;

use crate::io_uring_common::{
    BgidAllocError, BufferRingConfig, BufferRingError, IoUringConfig,
    IoUringKernelInfo, RegisteredBufferStats, RegisteredBufferStatus,
    SharedCompletion, SharedRingConfig,
};

/// Backend handle returned by [`IoUringBackend::build_ring`].
///
/// Opaque on every platform; carries the kernel ring handle on Linux
/// and a unit placeholder on the stub. Lifetime is tied to the
/// `&self` backend so a Linux ring cannot outlive the registration
/// state stored in the impl.
pub trait RingHandle: Send {
    /// Returns the SQ depth this ring was built with.
    fn sq_entries(&self) -> u32;

    /// Returns whether the kernel honoured `IORING_SETUP_SQPOLL`.
    /// Always `false` on the stub.
    fn sqpoll_active(&self) -> bool;
}

/// Typed submission entry. See section 3 for the type-vs-raw-sqe
/// trade-off and the recommended enum encoding.
#[derive(Debug)]
pub enum SubmissionEntry<'a> {
    /// `IORING_OP_READ` at the given offset.
    Read {
        fd: c_int,
        buf: &'a mut [u8],
        offset: u64,
        user_data: u64,
    },
    /// `IORING_OP_WRITE` at the given offset.
    Write {
        fd: c_int,
        buf: &'a [u8],
        offset: u64,
        user_data: u64,
    },
    /// `IORING_OP_READ_FIXED` into a pre-registered buffer slot.
    ReadFixed {
        fd: c_int,
        buf_index: u16,
        buf_ptr: *mut u8,
        len: u32,
        offset: u64,
        user_data: u64,
    },
    /// `IORING_OP_WRITE_FIXED` from a pre-registered buffer slot.
    WriteFixed {
        fd: c_int,
        buf_index: u16,
        buf_ptr: *const u8,
        len: u32,
        offset: u64,
        user_data: u64,
    },
    /// `IORING_OP_RECV` for socket reads.
    Recv {
        fd: c_int,
        buf: &'a mut [u8],
        user_data: u64,
    },
    /// `IORING_OP_SEND` for socket writes.
    Send {
        fd: c_int,
        buf: &'a [u8],
        user_data: u64,
    },
    /// `IORING_OP_SEND_ZC` for zero-copy socket writes (Linux 6.0+).
    SendZc {
        fd: c_int,
        buf: &'a [u8],
        user_data: u64,
    },
    /// `IORING_OP_FSYNC` durability barrier.
    Fsync { fd: c_int, user_data: u64 },
    /// `IORING_OP_POLL_ADD` readiness gate (`POLLIN` / `POLLOUT`).
    PollAdd {
        fd: c_int,
        events: u32,
        user_data: u64,
    },
    /// `IORING_OP_LINK_TIMEOUT` paired with a preceding POLL_ADD.
    LinkTimeout {
        timeout: Duration,
        user_data: u64,
    },
    /// `IORING_OP_STATX` async metadata lookup.
    Statx {
        dirfd: c_int,
        pathname: &'a CStr,
        flags: i32,
        mask: u32,
        statx_buf: &'a mut [u8; 256],
        user_data: u64,
    },
    /// `IORING_OP_RENAMEAT` (RENAMEAT2 with flags).
    Renameat2 {
        old_dirfd: c_int,
        old_path: &'a CStr,
        new_dirfd: c_int,
        new_path: &'a CStr,
        flags: u32,
        user_data: u64,
    },
    /// `IORING_OP_LINKAT` hard-link creation.
    Linkat {
        old_dirfd: c_int,
        old_path: &'a CStr,
        new_dirfd: c_int,
        new_path: &'a CStr,
        flags: i32,
        user_data: u64,
    },
    /// `IORING_OP_ASYNC_CANCEL` matched by `user_data`.
    CancelByUserData { target_user_data: u64, user_data: u64 },
    /// `IORING_OP_ASYNC_CANCEL` matched by fd (5.19+).
    CancelByFd { fd: c_int, user_data: u64 },
}

/// Opaque submission token returned by [`IoUringBackend::submit_one`].
///
/// Carries the `user_data` field assigned by the kernel so callers can
/// correlate the resulting CQE without re-encoding the tag themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmissionToken {
    /// The `user_data` value the submitted SQE will carry.
    pub user_data: u64,
}

/// Typed completion entry yielded by [`IoUringBackend::drain_completions`].
#[derive(Debug, Clone, Copy)]
pub struct CompletionEntry {
    /// `user_data` echoed from the originating SQE.
    pub user_data: u64,
    /// CQE `res` field; positive = success payload, negative = -errno.
    pub result: i32,
    /// CQE `flags` field (carries provided-buffer id when applicable).
    pub flags: u32,
}

/// Buffer group id returned by [`IoUringBackend::register_buffers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferGroupId(pub u16);

/// io_uring kernel opcode identifiers. Mirrors the SQE op byte; only
/// opcodes the codebase actually submits are enumerated. See
/// `docs/design/wpg-7-iouring-opcode-inventory.md` for the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Read = 22,
    Write = 23,
    ReadFixed = 4,
    WriteFixed = 5,
    Recv = 27,
    Send = 26,
    SendZc = 52,
    Fsync = 3,
    PollAdd = 6,
    LinkTimeout = 15,
    Statx = 21,
    Renameat = 35,
    Linkat = 39,
    AsyncCancel = 14,
}

/// Cross-platform io_uring backend trait. One impl per platform.
///
/// All methods are infallible at the dispatch site (no `cfg`-gated
/// arms in callers); the non-Linux impl returns
/// [`IoUringError::Unsupported`] uniformly. The Linux impl forwards to
/// the existing `crate::io_uring::*` machinery and must inline to a
/// direct call sequence per IUS-7.b (zero-cost guarantee, separate
/// doc).
pub trait IoUringBackend: Send + Sync {
    /// Concrete ring handle returned by [`Self::build_ring`].
    type Ring: RingHandle;

    // -- availability ------------------------------------------------

    /// Returns `true` when the backend can perform real submissions.
    /// Mirrors [`crate::io_uring_common::IoBackend::is_available`].
    fn is_available(&self) -> bool;

    /// Human-readable reason for the current availability state.
    fn availability_reason(&self) -> String;

    /// Returns `true` when SQPOLL was requested but fell back.
    fn sqpoll_fell_back(&self) -> bool;

    /// Returns structured kernel info for `--version` diagnostics.
    fn kernel_info(&self) -> IoUringKernelInfo;

    // -- ring lifecycle ----------------------------------------------

    /// Builds a ring from `config`. Errors on stub or on kernel reject.
    fn build_ring(&self, config: &IoUringConfig) -> Result<Self::Ring, IoUringError>;

    // -- submission / completion -------------------------------------

    /// Submits one SQE. The returned token carries the `user_data`
    /// the kernel will echo on the CQE.
    fn submit_one(
        &self,
        ring: &mut Self::Ring,
        sqe: SubmissionEntry<'_>,
    ) -> Result<SubmissionToken, IoUringError>;

    /// Submits a batch in one syscall and returns the assigned tokens
    /// in order. Equivalent to `submit_one` in a loop followed by a
    /// single `submit_and_wait(0)`.
    fn submit_batch<'a, I>(
        &self,
        ring: &mut Self::Ring,
        sqes: I,
    ) -> Result<Vec<SubmissionToken>, IoUringError>
    where
        I: IntoIterator<Item = SubmissionEntry<'a>>;

    /// Issues `io_uring_enter` waiting for at least `wait_for` CQEs.
    /// Returns the number of new completions the kernel posted.
    fn submit_and_wait(
        &self,
        ring: &mut Self::Ring,
        wait_for: usize,
    ) -> Result<usize, IoUringError>;

    /// Drains every ready completion. The iterator borrows the ring
    /// mutably so the caller cannot submit while draining.
    fn drain_completions<'a>(
        &self,
        ring: &'a mut Self::Ring,
    ) -> Box<dyn Iterator<Item = CompletionEntry> + 'a>;

    // -- buffer / file registration ----------------------------------

    /// Registers fixed buffers (`IORING_REGISTER_BUFFERS`). Returns
    /// the assigned group id; the buffers stay registered until
    /// [`Self::unregister_buffers`] or ring drop.
    fn register_buffers(
        &self,
        ring: &mut Self::Ring,
        bufs: &[&[u8]],
    ) -> Result<BufferGroupId, IoUringError>;

    /// Unregisters a previously-registered buffer group.
    fn unregister_buffers(
        &self,
        ring: &mut Self::Ring,
        id: BufferGroupId,
    ) -> Result<(), IoUringError>;

    /// Registers file descriptors (`IORING_REGISTER_FILES`). Slots are
    /// addressed by index in subsequent SQEs.
    fn register_files(
        &self,
        ring: &mut Self::Ring,
        fds: &[c_int],
    ) -> Result<(), IoUringError>;

    /// Unregisters every file slot in the ring.
    fn unregister_files(&self, ring: &mut Self::Ring) -> Result<(), IoUringError>;

    /// Registers a provided-buffer ring (`IORING_REGISTER_PBUF_RING`).
    fn register_provided_buffer_ring(
        &self,
        ring: &mut Self::Ring,
        config: BufferRingConfig,
    ) -> Result<BufferGroupId, BufferRingError>;

    /// Returns the per-ring registered-buffer stats. Always zeroed on
    /// the stub.
    fn registered_buffer_stats(&self, ring: &Self::Ring) -> RegisteredBufferStats;

    /// Returns the registered-buffer status (live / disabled /
    /// registration-failed) for diagnostic logging.
    fn registered_buffer_status(&self, ring: &Self::Ring) -> RegisteredBufferStatus;

    // -- per-opcode kernel probes ------------------------------------

    /// Queries the kernel `IORING_REGISTER_PROBE` for opcode support.
    /// Result is cached per backend instance.
    fn probe_op(&self, op: Opcode) -> bool;

    /// Pre-computed probe shortcuts for hot paths.
    fn statx_supported(&self) -> bool {
        self.probe_op(Opcode::Statx)
    }
    fn linkat_supported(&self) -> bool {
        self.probe_op(Opcode::Linkat)
    }
    fn renameat2_supported(&self) -> bool {
        self.probe_op(Opcode::Renameat)
    }
    fn send_zc_supported(&self) -> bool {
        self.probe_op(Opcode::SendZc)
    }
    fn pbuf_ring_supported(&self) -> bool;
    fn cancel_supported(&self) -> bool {
        self.probe_op(Opcode::AsyncCancel)
    }
    fn cancel_by_fd_supported(&self) -> bool;

    // -- bgid allocator (per-process namespace) ----------------------

    /// Allocates the next buffer-group id from the process-wide
    /// namespace. Returns [`BgidAllocError::Exhausted`] when the
    /// pool is drained; the stub always returns exhausted.
    fn allocate_bgid(&self) -> Result<u16, BgidAllocError>;

    /// Releases a previously-allocated bgid back to the pool.
    fn deallocate_bgid(&self, bgid: u16);

    /// Returns the number of bgids still issuable. Always 0 on stub.
    fn bgid_remaining(&self) -> u32;

    // -- blocking convenience wrappers (mirror existing helpers) -----

    /// Blocking `IORING_OP_STATX` for one path; returns `Unsupported`
    /// on the stub.
    fn submit_statx_blocking(
        &self,
        dirfd: c_int,
        pathname: &CStr,
        flags: i32,
        mask: u32,
    ) -> Result<(), IoUringError>;

    /// Blocking `IORING_OP_STATX` for a batch of paths; per-path
    /// result vector. Stub returns one `Unsupported` per path.
    fn submit_statx_batch(
        &self,
        paths: &[&Path],
        follow_symlinks: bool,
    ) -> Result<Vec<io::Result<()>>, IoUringError>;

    /// Blocking `IORING_OP_LINKAT`. Stub returns `Unsupported`.
    fn submit_linkat_blocking(
        &self,
        old_dirfd: c_int,
        old_path: &CStr,
        new_dirfd: c_int,
        new_path: &CStr,
        flags: i32,
    ) -> Result<(), IoUringError>;

    /// Blocking `IORING_OP_RENAMEAT`. Stub returns `Unsupported`.
    fn submit_renameat2_blocking(
        &self,
        old_dirfd: c_int,
        old_path: &CStr,
        new_dirfd: c_int,
        new_path: &CStr,
        flags: u32,
    ) -> Result<(), IoUringError>;

    // -- session / shared-ring construction --------------------------

    /// Builds the per-session ring pool. Returns `Unsupported` on
    /// stub; on Linux constructs the existing `SessionRingPool`.
    fn build_session_pool(
        &self,
        config: SharedRingConfig,
    ) -> Result<Box<dyn SessionPool>, IoUringError>;

    /// Builds a shared reader+writer ring (single-fd pair). Returns
    /// `Unsupported` on stub.
    fn build_shared_ring(
        &self,
        reader_fd: c_int,
        writer_fd: c_int,
        config: &SharedRingConfig,
    ) -> Result<Box<dyn SharedRingHandle>, IoUringError>;

    // -- file / socket factory entry points --------------------------

    /// Opens a file for reading through the registered-buffer write
    /// path when available; falls back to standard I/O otherwise.
    /// Used by callers that want one entry point instead of branching
    /// on `is_available` themselves.
    fn open_reader(
        &self,
        path: &Path,
        config: &IoUringConfig,
    ) -> Result<Box<dyn crate::traits::FileReader + Send>, IoUringError>;

    /// Opens a file for writing; symmetric to `open_reader`.
    fn open_writer(
        &self,
        path: &Path,
        config: &IoUringConfig,
    ) -> Result<Box<dyn crate::traits::FileWriter + Send>, IoUringError>;

    /// Wraps an existing `File` as an io_uring writer respecting the
    /// caller's policy.
    fn writer_from_file(
        &self,
        file: File,
        buffer_capacity: usize,
        config: &IoUringConfig,
    ) -> Result<Box<dyn crate::traits::FileWriter + Send>, IoUringError>;

    // -- batched disk-commit helper ----------------------------------

    /// Builds the batched disk-commit writer used by the receiver
    /// disk thread. Returns `Unsupported` on stub.
    fn build_disk_batch(
        &self,
        config: &IoUringConfig,
    ) -> Result<Box<dyn DiskBatch + Send>, IoUringError>;
}

/// Object-safe session-pool handle returned by
/// [`IoUringBackend::build_session_pool`].
pub trait SessionPool: Send + Sync {
    fn ring_count(&self) -> usize;
    fn acquire(&self) -> Option<Box<dyn SessionLease + '_>>;
}

/// Object-safe RAII lease over a pooled ring.
pub trait SessionLease {
    fn slot(&self) -> usize;
}

/// Object-safe shared-ring handle.
pub trait SharedRingHandle: Send {
    fn reader_slot(&self) -> i32;
    fn writer_slot(&self) -> i32;
    fn poll_add_supported(&self) -> bool;
    fn has_registered_buffers(&self) -> bool;
    fn submit_read(&mut self, op_id: u64, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn submit_send(&mut self, op_id: u64, data: &[u8]) -> io::Result<()>;
    fn submit_poll_write(&mut self, op_id: u64) -> io::Result<()>;
    fn submit_and_wait(&mut self, wait_for: usize) -> io::Result<usize>;
    fn reap(&mut self) -> io::Result<Vec<SharedCompletion>>;
}

/// Object-safe batched disk-commit writer.
pub trait DiskBatch: io::Write {
    fn begin_file(&mut self, file: File) -> io::Result<()>;
    fn write_data(&mut self, data: &[u8]) -> io::Result<()>;
    fn commit_file(&mut self, do_fsync: bool) -> io::Result<(File, u64)>;
    fn bytes_written(&self) -> u64;
    fn bytes_written_with_pending(&self) -> u64;
}
```

**Method count.** Counting public methods on the four traits in this
section: `IoUringBackend` has 38 methods (2 availability + 1
`kernel_info` + 1 `sqpoll_fell_back` + 1 `build_ring` + 4 submission /
completion + 7 buffer/file registration + 8 probe + 3 bgid + 4
blocking wrappers + 2 session/shared-ring + 3 file/socket factory + 1
disk-batch). `RingHandle` adds 2, `SessionPool` + `SessionLease` add
3, `SharedRingHandle` adds 9, `DiskBatch` adds 5. **Trait method
count: 57 total across the five traits; 38 on `IoUringBackend`
itself.**

## 2. Error type

```rust
use std::io;

/// Unified error type returned by every `IoUringBackend` method.
///
/// Designed so callers can `match` on the variant for graceful
/// fallback (`Unsupported` -> standard I/O) without inspecting an
/// inner `io::Error` kind.
#[derive(Debug, thiserror::Error)]
pub enum IoUringError {
    /// The backend is not built for this platform or `is_available`
    /// returned `false` at runtime. Callers should fall back to the
    /// standard I/O path. Always returned by the non-Linux stub.
    #[error("io_uring: unsupported on this platform")]
    Unsupported,

    /// The kernel reports a version below the minimum required for
    /// the requested feature.
    #[error("io_uring: kernel {actual:?} below required {required:?}")]
    KernelTooOld {
        /// Detected (major, minor) at probe time.
        actual: (u32, u32),
        /// Minimum kernel required for the requested feature.
        required: (u32, u32),
    },

    /// The submission queue is full and `submit_and_wait` is required
    /// before the SQE can be queued.
    #[error("io_uring: submission queue full")]
    SubmissionFull,

    /// The kernel returned `EOPNOTSUPP` for the requested opcode on
    /// this kernel build.
    #[error("io_uring: opcode {opcode:?} not supported by this kernel")]
    OpcodeUnsupported {
        /// The opcode the probe rejected.
        opcode: super::Opcode,
    },

    /// Buffer-group registration failed; carries the inner reason.
    #[error("io_uring: buffer ring registration failed: {0}")]
    BufferRing(#[from] crate::io_uring_common::BufferRingError),

    /// Bgid allocation failed; pool is exhausted.
    #[error("io_uring: bgid pool exhausted")]
    BgidExhausted(#[from] crate::io_uring_common::BgidAllocError),

    /// SQPOLL was requested but the kernel rejected setup (typically
    /// `EPERM` without `CAP_SYS_NICE`). The backend has already fallen
    /// back to non-SQPOLL submission; this variant is informational.
    #[error("io_uring: SQPOLL setup rejected, fell back to regular submission")]
    SqpollFellBack,

    /// Underlying `io::Error` from the kernel or the `io-uring` crate.
    /// Used for `EBADF`, `EINVAL`, and other errors not modelled
    /// explicitly above. Use `From<io::Error>` for ergonomic `?`.
    #[error("io_uring: {0}")]
    IoError(#[from] io::Error),
}

impl IoUringError {
    /// Convenience for callers that want to convert to `io::Error`
    /// for legacy APIs. `Unsupported` maps to `io::ErrorKind::Unsupported`.
    pub fn into_io_error(self) -> io::Error {
        match self {
            IoUringError::Unsupported => {
                io::Error::new(io::ErrorKind::Unsupported, "io_uring not available")
            }
            IoUringError::IoError(e) => e,
            other => io::Error::new(io::ErrorKind::Other, other.to_string()),
        }
    }
}
```

## 3. Submission entry: typed enum vs raw `io_uring_sqe`

Two viable encodings:

| Encoding | Memory overhead | Inlining | Safety | Code-size cost |
|----------|-----------------|----------|--------|----------------|
| Typed enum (recommended) | One tag byte + largest variant (~64 B) per SQE in flight | Compiles to direct sqe writes when the variant is statically known at the call site (e.g., `submit_one(Read{...})` inlines to one SQE init) | Variants enforce type-safe arg matching (no `union` foot-guns); fd / buf / offset cannot mismatch | Match dispatch in the Linux impl - one arm per opcode, but each arm shrinks to a constant-fold under `-Copt-level=3` |
| Raw `io_uring_sqe` mirror | 64 B fixed | No tag dispatch | Unsafe-ish: callers fill arbitrary fields | Larger public surface; couples the trait to the C UAPI layout |

**Recommendation: typed enum.** The IUS-7.b zero-cost spec (separate
doc) will verify via `cargo asm` that `IoUringBackend::submit_one`
inlines on Linux with the variant tag folded away, matching the
current direct-sqe path within one instruction. The non-Linux impl
discards every variant and returns `Err(IoUringError::Unsupported)` -
the match itself is constant-folded out by the optimiser because the
function body is a single return.

The trade-off: adding a new opcode requires extending the enum and
the Linux match arm, where the raw form would let callers fill an SQE
without trait changes. The upstream-fidelity rule favours the
explicit enum: every opcode the codebase submits is enumerated in
`docs/design/wpg-7-iouring-opcode-inventory.md`, and gating new
opcodes through trait additions is the same review discipline the
current cross-platform stub requires.

## 4. Completion entry surface

The `CompletionEntry` struct (section 1) is intentionally a thin
mirror of the kernel CQE:

- `user_data: u64` - echoed from the SQE, used by callers to
  correlate. Tag encoding (high 8 bits) is reused via
  `crate::io_uring_common::OpTag::decode` so completion handlers do
  not need to know whether the backend submitted a `Read` or a
  `ReadFixed`.
- `result: i32` - positive = bytes / fd, negative = -errno. Callers
  call `io::Error::from_raw_os_error(-self.result)` when negative.
- `flags: u32` - holds provided-buffer id when
  `IORING_CQE_F_BUFFER` is set; opaque otherwise.

A typed completion enum (`CompletionEntry::Read { user_data, bytes
}`) was considered and rejected: the kernel CQE shape is uniform and
the typed projection forces an extra match in the consumer that the
existing `OpTag::decode` already provides. The struct form composes
with the existing demultiplexer in
`crates/fast_io/src/io_uring/shared_ring.rs` without changing
`SharedCompletion`.

## 5. Linux impl - `LinuxIoUringBackend`

The Linux impl is a thin forwarder that lives at
`crates/fast_io/src/io_uring/backend_impl.rs`. Every method delegates
to the existing wrappers; **no new logic** lands in the impl - it is
mechanical plumbing only.

```rust
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub struct LinuxIoUringBackend {
    /// Cached kernel info (populated on first call to
    /// `is_available`).
    kernel_info: std::sync::OnceLock<IoUringKernelInfo>,
    /// Cached opcode probes keyed by opcode number.
    probe_cache: std::sync::OnceLock<u128>,
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
impl IoUringBackend for LinuxIoUringBackend {
    type Ring = super::shared_ring::SharedRing; // or a thin newtype

    fn is_available(&self) -> bool {
        super::config::is_io_uring_available()
    }

    fn availability_reason(&self) -> String {
        super::config::config_detail::io_uring_availability_reason()
    }

    fn sqpoll_fell_back(&self) -> bool {
        super::config::sqpoll_fell_back()
    }

    fn submit_one(
        &self,
        ring: &mut Self::Ring,
        sqe: SubmissionEntry<'_>,
    ) -> Result<SubmissionToken, IoUringError> {
        // Each arm calls the existing builder in `linkat`, `statx`,
        // `renameat2`, `cancel`, `send_zc`, or the raw `io-uring`
        // crate for Read/Write/Send/Recv. IUS-7.b verifies inlining.
        match sqe {
            SubmissionEntry::Statx { dirfd, pathname, flags, mask, statx_buf, user_data } => {
                super::statx::build_statx_sqe(&mut super::statx::StatxArgs {
                    dirfd, pathname, flags, mask, statx_buf,
                })?;
                Ok(SubmissionToken { user_data })
            }
            // ... one arm per opcode
        }
    }

    fn probe_op(&self, op: Opcode) -> bool {
        let bits = *self.probe_cache.get_or_init(|| {
            super::config::config_detail::io_uring_kernel_info().supported_ops as u128
        });
        bits & (1u128 << (op as u8)) != 0
    }

    // ... 35 more methods, all 1-3 line forwarders
}
```

The impl is a single file, projected at ~400 LoC end-to-end (38
methods averaging ~10 LoC each). No new behaviour, no new tests
beyond a smoke test asserting the impl reaches every existing
wrapper.

## 6. Non-Linux impl - `StubIoUringBackend`

The stub impl replaces the entire 2,422-LoC mirror tree. Every method
returns `Err(IoUringError::Unsupported)` or its typed equivalent
(`Ok(false)` for probes, zeroed stats, etc.).

```rust
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
pub struct StubIoUringBackend;

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
pub struct StubRing;

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
impl RingHandle for StubRing {
    fn sq_entries(&self) -> u32 { 0 }
    fn sqpoll_active(&self) -> bool { false }
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
impl IoUringBackend for StubIoUringBackend {
    type Ring = StubRing;

    fn is_available(&self) -> bool { false }

    fn availability_reason(&self) -> String {
        "io_uring: disabled (not built for this target)".to_string()
    }

    fn sqpoll_fell_back(&self) -> bool { false }

    fn kernel_info(&self) -> IoUringKernelInfo {
        IoUringKernelInfo {
            available: false,
            kernel_major: None,
            kernel_minor: None,
            supported_ops: 0,
            pbuf_ring_supported: false,
            reason: self.availability_reason(),
        }
    }

    fn build_ring(&self, _config: &IoUringConfig) -> Result<Self::Ring, IoUringError> {
        Err(IoUringError::Unsupported)
    }

    fn probe_op(&self, _op: Opcode) -> bool { false }
    fn pbuf_ring_supported(&self) -> bool { false }
    fn cancel_by_fd_supported(&self) -> bool { false }

    fn allocate_bgid(&self) -> Result<u16, BgidAllocError> {
        Err(BgidAllocError::Exhausted { fresh_used: 0, free_list_len: 0 })
    }
    fn deallocate_bgid(&self, _bgid: u16) {}
    fn bgid_remaining(&self) -> u32 { 0 }

    fn submit_one(&self, _r: &mut Self::Ring, _s: SubmissionEntry<'_>) -> Result<SubmissionToken, IoUringError> {
        Err(IoUringError::Unsupported)
    }
    // ... every other method is one line of `Err(IoUringError::Unsupported)`
    //     or `Ok(false)` / zeroed stats
}
```

**Projected size.** The stub impl block is ~150 LoC (one return per
method, no per-opcode arms because every variant returns the same
error). Adding the marker structs, trait re-exports, and the kernel
info constructor brings the **total stub crate to ~200 LoC** -
roughly a 12x reduction from the current 2,422 LoC across 21 files,
collapsed to one file.

The shared types (`IoUringConfig`, `IoUringKernelInfo`, `OpTag`,
`SharedCompletion`, `BufferRingConfig`, `RegisteredBufferStats`, all
constants) stay in `io_uring_common.rs` exactly as they are today and
both impls re-export them unchanged.

## 7. Public exposure

The trait + types live in `fast_io::io_uring::backend`:

```rust
// crates/fast_io/src/io_uring/backend.rs
pub trait IoUringBackend { /* section 1 */ }
pub trait RingHandle { /* section 1 */ }
pub trait SessionPool { /* section 1 */ }
pub trait SessionLease { /* section 1 */ }
pub trait SharedRingHandle { /* section 1 */ }
pub trait DiskBatch { /* section 1 */ }

pub enum SubmissionEntry<'a> { /* section 1 */ }
pub struct SubmissionToken { /* section 1 */ }
pub struct CompletionEntry { /* section 1 */ }
pub struct BufferGroupId(pub u16);
pub enum Opcode { /* section 1 */ }
pub enum IoUringError { /* section 2 */ }
```

Re-exported from `crates/fast_io/src/lib.rs`:

```rust
pub use io_uring::backend::{
    BufferGroupId, CompletionEntry, DiskBatch, IoUringBackend, IoUringError,
    Opcode, RingHandle, SessionLease, SessionPool, SharedRingHandle,
    SubmissionEntry, SubmissionToken,
};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub use io_uring::backend::LinuxIoUringBackend;
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
pub use io_uring::backend::StubIoUringBackend;
```

Callers replace direct `IoUring`-handle references with one of:

- `impl IoUringBackend` (generic - preferred for hot paths; LLVM
  inlines).
- `Arc<dyn IoUringBackend<Ring = R>>` (object-safe form for storage in
  long-lived state). Note: associated types complicate `dyn`; if
  trait-object use is required, the `Self::Ring` projection is folded
  into a `BoxedRing` newtype that erases the associated type. The
  IUR-2 per-thread rings work (section 9) is the natural place to
  introduce the newtype.

The existing free functions (`is_io_uring_available`,
`io_uring_availability_reason`, `pbuf_ring_supported`, ...) stay as
thin shims forwarding to the backend instance so that current callers
do not have to migrate in one shot. Deprecation of the free functions
is out of scope for IUS-7 / IUS-8.

## 8. Migration ordering

| ID | Title | Depends on | Scope |
|----|-------|------------|-------|
| **IUS-7.a** | Spec `IoUringBackend` trait surface (this doc) | - | Design only |
| IUS-7.b | Spec zero-cost guarantee details | IUS-7.a | Design only - inlining proof, codegen asserts |
| IUS-8.a | Define `IoUringBackend` trait surface (impl of this spec) | IUS-7.a, IUS-7.b | New code in `crates/fast_io/src/io_uring/backend.rs`; no callers migrated yet |
| IUS-8.b | Implement `IoUringBackend` for Linux `IoUring` | IUS-8.a | New file `backend_impl.rs`; existing wrappers stay; one smoke test asserting the trait covers every wrapper |
| IUS-8.c | Implement `IoUringBackend` stub for non-Linux + replace `io_uring_stub.rs` | IUS-8.b | Delete `crates/fast_io/src/io_uring_stub/` tree; replace with single-file `io_uring/backend_stub.rs`; CI matrix verifies Windows + macOS + Linux-no-feature builds clean |

No caller migration is part of IUS-7 / IUS-8. Caller migration is a
separate initiative (provisional **IUS-9**) once the trait is in
place and the zero-cost guarantee is verified.

## 9. Open issues

### 9.1 Where does the backend instance live?

Three options for the backend handle's storage:

1. **Process-wide `OnceLock<Arc<dyn IoUringBackend>>`.** Simple,
   matches the existing process-wide availability cache, but ties
   every test to the same backend (mock injection becomes intrusive).
2. **`Arc<dyn IoUringBackend>` plumbed through `CoreConfig`.**
   Aligns with the existing dependency-inversion design pattern used
   throughout the workspace (traits define interfaces, implementations
   are swappable). Adds one field to `CoreConfig`; mocks can be
   injected at construction time.
3. **Per-thread `OnceLock<Arc<dyn IoUringBackend>>`.** Coordinates
   directly with the IUR-2 per-thread rings work; each thread holds
   its own backend handle which in turn owns the thread's ring.
   Preferred shape for the per-thread-rings end state.

**Recommendation:** start with option 2 for the IUS-8 land; revisit
option 3 when IUR-2 ships per-thread rings. The migration is
non-breaking because all backend operations are on `&self`.

### 9.2 Trait object vs generic dispatch

The trait has one associated type (`Ring`). For `dyn IoUringBackend`
use the impl must erase the associated type via a `BoxedRing`
wrapper:

```rust
pub struct BoxedRing(Box<dyn RingHandle>);
pub trait DynIoUringBackend: Send + Sync {
    fn submit_one(&self, ring: &mut BoxedRing, sqe: SubmissionEntry<'_>) -> Result<SubmissionToken, IoUringError>;
    // ... all 38 methods, retyped to take `BoxedRing`
}

impl<T: IoUringBackend<Ring = R>, R: RingHandle + 'static> DynIoUringBackend for T {
    // forward through `BoxedRing` downcast
}
```

This is purely additive - generic users skip the dyn layer entirely.
The IUS-8.a deliverable should provide both the generic trait and
the dyn-safe adapter so the IUR-2 storage decision can take whichever
form makes per-thread integration cleanest.

### 9.3 Probe caching policy

`probe_op` cached in `OnceLock<u128>` (section 5) packs every opcode
< 128 into one bitmap. This is sufficient for every opcode the
codebase submits today (max is `SEND_ZC = 52`). If a future opcode
exceeds 127 the cache widens to a `RwLock<HashMap<u8, bool>>` - one
behavioural change ringfenced inside the Linux impl.

### 9.4 SubmissionEntry lifetime ergonomics

The enum holds borrows (`&[u8]`, `&mut [u8]`, `&CStr`) so the typical
call shape is `submit_one(&mut ring, SubmissionEntry::Read { buf:
&mut buf, ... })`. For batched submission where the caller owns the
buffers in a vector, the iterator API on `submit_batch` requires the
buffers to outlive the iterator. The existing
`IoUringDiskBatch::write_data` pattern (write to internal buffer,
flush on rotation) maps cleanly. Cross-thread buffer ownership (the
PIP-3 / SLC-1 work) requires the buffer borrow to extend through the
worker - this is the same constraint the existing wrappers already
enforce via `RegisteredBufferSlot<'a>` lifetimes.

### 9.5 Feature-gated methods

`send_zc` is gated behind `iouring-send-zc` today
(`crates/fast_io/src/io_uring/send_zc.rs`). The trait exposes
`submit_one(SendZc { ... })` unconditionally; the Linux impl returns
`Err(IoUringError::OpcodeUnsupported)` when the feature is off, so
callers always see one error path instead of `cfg`-gating each call
site. `iouring-data-reads` / `iouring-data-writes` follow the same
pattern - the trait methods exist unconditionally; the Linux impl
returns the configured error variant when the feature is absent.

### 9.6 Stub crate name

The current crate path is `crates/fast_io/src/io_uring_stub/` (with
trailing `/`, 21 files). Post-IUS-8.c the path becomes
`crates/fast_io/src/io_uring/backend_stub.rs` (single file inside
the `io_uring` module tree). The old `io_uring_stub` re-export path
stays as a deprecated alias for one release cycle so external callers
(none in-tree today; future plugin crates) have a migration window.

---

**Trait method count:** 57 across the five traits in section 1; 38
methods on `IoUringBackend` itself. The Linux impl forwards each
method to existing wrappers (no new logic). The non-Linux impl
collapses the 2,422 LoC current stub tree to a single file of ~200
LoC (one return per method).
