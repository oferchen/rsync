//! Windows Registered I/O (RIO) wrappers for high-throughput daemon socket I/O.
//!
//! RIO is a Winsock extension introduced in Windows 8 / Server 2012 that
//! pre-registers user-mode buffer pools with the kernel, eliminating the
//! per-call page-pinning overhead that `WSARecv` / `WSASend` pay on every
//! overlapped operation. For a daemon serving many concurrent transfers, this
//! shaves the dominant overhead of the IOCP socket path, at the cost of a
//! considerably more involved API surface (registered buffer pools, registered
//! completion queues, dedicated request queues per socket).
//!
//! # Scope
//!
//! This module ships the foundation pieces required to wire RIO into the
//! daemon socket dispatch site behind an opt-in env-var gate:
//!
//! - [`try_init_rio`] - resolves the RIO extension function table via
//!   `WSAIoctl(SIO_GET_MULTIPLE_EXTENSION_FUNCTION_POINTER)` and returns
//!   `Ok(None)` when RIO is unavailable on the host. Older Windows builds (or
//!   builds without the Network Direct provider) silently fall back to the
//!   regular [`super::socket`] path.
//! - [`RioBufferPool`] - allocates a single contiguous buffer block (default
//!   1 MiB), registers it via `RIORegisterBuffer`, and hands out
//!   [`RegisteredBuffer`] descriptors carved out of that block as `RIO_BUF`s
//!   ready to be passed to `RIOSend` / `RIOReceive`.
//! - [`RioCompletionQueue`] - thin wrapper around `RIOCreateCompletionQueue`
//!   plus `RIODequeueCompletion` with `RIOCloseCompletionQueue` invoked on
//!   drop.
//! - [`rio_enabled_from_env`] - parses the `OC_RSYNC_WINDOWS_RIO=auto|on|off`
//!   knob. Default is `off` so the daemon continues to use the standard IOCP
//!   socket path until the bench tasks (NET-RIO.4 / NET-RIO.5) flip the
//!   default.
//!
//! # Upstream reference
//!
//! Upstream rsync does **not** use RIO. The Cygwin shim it relies on for
//! Winsock does not expose RIO at all, so this is an oc-rsync-specific
//! Windows-only optimisation. There is no protocol impact: RIO is purely a
//! local syscall replacement for `WSARecv` / `WSASend` over a registered
//! buffer; the bytes on the wire are identical.
//!
//! # Safety boundary
//!
//! All `unsafe` lives inside this file. Public APIs are safe; consumer crates
//! (`daemon`, `core`, `transport`) call into them without any `#[allow(unsafe_code)]`
//! escape hatch. The cross-platform stub at `iocp_stub::rio` mirrors every
//! public type so callers can name them on non-Windows targets behind a
//! runtime availability check.

use std::ffi::c_void;
use std::io;
use std::mem::MaybeUninit;
use std::os::windows::io::RawSocket;
use std::ptr;
use std::sync::Arc;

use windows_sys::Win32::Networking::WinSock::{
    INVALID_SOCKET, RIO_BUF, RIO_BUFFERID, RIO_CQ, RIO_EXTENSION_FUNCTION_TABLE,
    RIO_NOTIFICATION_COMPLETION, RIO_RQ, SIO_GET_MULTIPLE_EXTENSION_FUNCTION_POINTER, SOCKET,
    WSAID_MULTIPLE_RIO, WSAIoctl, WSASocketW,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::core::GUID;

/// Default registered-buffer-pool size handed to the kernel.
///
/// 1 MiB is large enough to cover the rsync multiplex layer's working set
/// (multiple in-flight 32 KiB frames per direction) while small enough to fit
/// comfortably under the system non-paged-pool budget on a workstation. The
/// actual size is configurable through [`RioBufferPool::with_capacity`].
pub const DEFAULT_RIO_POOL_BYTES: usize = 1024 * 1024;

/// Per-buffer slot size. Each [`RegisteredBuffer`] handed out by
/// [`RioBufferPool::acquire`] is a contiguous region of this size carved from
/// the registered block.
pub const DEFAULT_RIO_SLOT_BYTES: usize = 32 * 1024;

/// Sentinel value returned by `RIORegisterBuffer` to indicate failure.
///
/// Microsoft documents the invalid-buffer sentinel as the all-ones bit
/// pattern (`RIO_INVALID_BUFFERID`). `RIO_BUFFERID` is `isize`; representing
/// the sentinel as `-1` matches the SDK header definition.
const RIO_INVALID_BUFFERID: RIO_BUFFERID = -1;
const RIO_INVALID_CQ: RIO_CQ = -1;

/// User-facing knob controlling whether the daemon attempts the RIO path.
///
/// Defaults to [`RioMode::Off`] until NET-RIO.4 benchmarks validate the
/// dispatcher and NET-RIO.5 flips the production default to `Auto`. The env
/// var is read via [`rio_enabled_from_env`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RioMode {
    /// RIO disabled regardless of availability. Daemon uses the existing
    /// IOCP `WSARecv` / `WSASend` socket path.
    Off,
    /// RIO is attempted: when the extension function table resolves the
    /// daemon switches to the RIO dispatcher; otherwise it transparently
    /// falls back to the IOCP path.
    Auto,
    /// RIO is required: callers should error out (or log loudly) when the
    /// extension is unavailable. Useful for benchmarking and CI gating.
    On,
}

impl Default for RioMode {
    fn default() -> Self {
        Self::Off
    }
}

/// Environment variable that selects the RIO mode at process start.
pub const RIO_ENV_VAR: &str = "OC_RSYNC_WINDOWS_RIO";

/// Parses the `OC_RSYNC_WINDOWS_RIO` env var into a [`RioMode`].
///
/// Returns the default mode (`Off`) for any value that is unset, empty, or
/// not one of `auto` / `on` / `off` (case-insensitive). Documented values:
///
/// | Value     | Meaning                                                     |
/// |-----------|-------------------------------------------------------------|
/// | unset     | RIO disabled (current default).                             |
/// | `off`     | Same as unset.                                              |
/// | `auto`    | Attempt RIO; fall back to IOCP when unavailable.            |
/// | `on`      | Require RIO; callers may refuse to start if unavailable.    |
///
/// The default lives in [`RioMode::default`] so a single source of truth
/// covers both the env-var parser and any in-process callers that build a
/// `RioMode` directly.
#[must_use]
pub fn rio_enabled_from_env() -> RioMode {
    parse_rio_env(std::env::var(RIO_ENV_VAR).ok().as_deref())
}

/// Pure parser for [`rio_enabled_from_env`], exposed for unit testing without
/// mutating the process environment.
#[doc(hidden)]
#[must_use]
pub fn parse_rio_env(value: Option<&str>) -> RioMode {
    match value.map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("on") => RioMode::On,
        Some(s) if s.eq_ignore_ascii_case("auto") => RioMode::Auto,
        _ => RioMode::Off,
    }
}

/// Resolved RIO extension function table.
///
/// Built by [`try_init_rio`] from the dispatch table populated by
/// `WSAIoctl(SIO_GET_MULTIPLE_EXTENSION_FUNCTION_POINTER, WSAID_MULTIPLE_RIO)`.
/// Every field is the unwrapped function pointer (the windows-sys
/// `LPFN_RIO*` aliases are `Option<unsafe extern "system" fn(...)>`; here we
/// strip the `Option` so callers do not have to revalidate non-null on
/// every dispatch).
#[derive(Clone, Copy)]
pub struct RioFunctions {
    register_buffer: NonNullRegisterBuffer,
    deregister_buffer: NonNullDeregisterBuffer,
    send: NonNullSend,
    receive: NonNullReceive,
    create_completion_queue: NonNullCreateCompletionQueue,
    close_completion_queue: NonNullCloseCompletionQueue,
    create_request_queue: NonNullCreateRequestQueue,
    dequeue_completion: NonNullDequeueCompletion,
    notify: NonNullNotify,
}

type NonNullRegisterBuffer = unsafe extern "system" fn(
    databuffer: windows_sys::core::PCSTR,
    datalength: u32,
) -> RIO_BUFFERID;
type NonNullDeregisterBuffer = unsafe extern "system" fn(bufferid: RIO_BUFFERID);
type NonNullSend = unsafe extern "system" fn(
    socketqueue: RIO_RQ,
    pdata: *const RIO_BUF,
    databuffercount: u32,
    flags: u32,
    requestcontext: *const c_void,
) -> windows_sys::core::BOOL;
type NonNullReceive = unsafe extern "system" fn(
    socketqueue: RIO_RQ,
    pdata: *const RIO_BUF,
    databuffercount: u32,
    flags: u32,
    requestcontext: *const c_void,
) -> windows_sys::core::BOOL;
type NonNullCreateCompletionQueue = unsafe extern "system" fn(
    queuesize: u32,
    notificationcompletion: *const RIO_NOTIFICATION_COMPLETION,
) -> RIO_CQ;
type NonNullCloseCompletionQueue = unsafe extern "system" fn(cq: RIO_CQ);
type NonNullCreateRequestQueue = unsafe extern "system" fn(
    socket: SOCKET,
    maxoutstandingreceive: u32,
    maxreceivedatabuffers: u32,
    maxoutstandingsend: u32,
    maxsenddatabuffers: u32,
    receivecq: RIO_CQ,
    sendcq: RIO_CQ,
    socketcontext: *const c_void,
) -> RIO_RQ;
type NonNullDequeueCompletion = unsafe extern "system" fn(
    cq: RIO_CQ,
    array: *mut windows_sys::Win32::Networking::WinSock::RIORESULT,
    arraysize: u32,
) -> u32;
type NonNullNotify = unsafe extern "system" fn(cq: RIO_CQ) -> i32;

impl RioFunctions {
    /// Returns true when the extension table was populated successfully.
    ///
    /// All `RioFunctions` instances handed back from [`try_init_rio`] are
    /// resolved by construction; this helper exists for symmetry with the
    /// stub module so cross-platform callers can write the same check.
    #[must_use]
    pub fn is_available(&self) -> bool {
        true
    }
}

impl std::fmt::Debug for RioFunctions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RioFunctions").finish_non_exhaustive()
    }
}

/// Attempts to resolve the RIO extension function table.
///
/// Returns:
///
/// - `Ok(Some(RioFunctions))` when the host kernel exposes RIO. Windows 8 /
///   Server 2012 or newer with the standard Microsoft Network Direct provider.
/// - `Ok(None)` when the host kernel does not expose RIO (older builds, or
///   container images with a stripped Winsock catalog). Callers should
///   transparently fall back to the IOCP `WSARecv` / `WSASend` path.
/// - `Err` for hard failures unrelated to RIO availability (cannot open a
///   probe socket, `WSAIoctl` returns an unexpected error). The daemon
///   should surface these because they indicate broken Winsock.
///
/// The probe creates a transient `AF_INET` `SOCK_STREAM` socket purely to
/// drive `WSAIoctl`; the socket is closed before this function returns.
///
/// # Errors
///
/// Returns `io::Error` for `WSASocketW` / `WSAIoctl` failures other than the
/// well-known "extension not present" codes (`WSAEINVAL`,
/// `WSAEOPNOTSUPP`), which both map to `Ok(None)`.
pub fn try_init_rio() -> io::Result<Option<RioFunctions>> {
    // Open a probe socket to drive WSAIoctl. RIO requires the socket be
    // created with WSA_FLAG_REGISTERED_IO (0x100) but the extension-pointer
    // lookup itself only needs a valid SOCKET handle of the right family.
    // SAFETY: WSASocketW with AF_INET / SOCK_STREAM is a standard probe;
    // returns INVALID_SOCKET on failure which we map to a portable error.
    const AF_INET: i32 = 2;
    const SOCK_STREAM: i32 = 1;
    const IPPROTO_TCP: i32 = 6;
    #[allow(unsafe_code)]
    let probe: SOCKET = unsafe { WSASocketW(AF_INET, SOCK_STREAM, IPPROTO_TCP, ptr::null(), 0, 0) };
    if probe == INVALID_SOCKET {
        return Err(io::Error::last_os_error());
    }

    let result = resolve_table(probe);

    // Close the probe socket regardless of dispatch outcome. closesocket()
    // is the standard Winsock teardown; ignoring its return value is the
    // documented pattern when the socket was never bound.
    // SAFETY: `probe` is a valid SOCKET returned by WSASocketW above.
    #[allow(unsafe_code)]
    unsafe {
        windows_sys::Win32::Networking::WinSock::closesocket(probe);
    }

    result
}

fn resolve_table(probe: SOCKET) -> io::Result<Option<RioFunctions>> {
    let mut table: MaybeUninit<RIO_EXTENSION_FUNCTION_TABLE> = MaybeUninit::zeroed();
    let mut bytes: u32 = 0;
    let guid: GUID = WSAID_MULTIPLE_RIO;

    // SAFETY: WSAIoctl with SIO_GET_MULTIPLE_EXTENSION_FUNCTION_POINTER
    // takes (in) a GUID and (out) writes RIO_EXTENSION_FUNCTION_TABLE
    // bytes. The output buffer is sized to the full struct. The kernel is
    // documented to write up to `cbSize` bytes; we keep the buffer alive
    // for the entire call.
    #[allow(unsafe_code)]
    let rc = unsafe {
        WSAIoctl(
            probe,
            SIO_GET_MULTIPLE_EXTENSION_FUNCTION_POINTER,
            (&guid as *const GUID).cast::<c_void>(),
            std::mem::size_of::<GUID>() as u32,
            table.as_mut_ptr().cast::<c_void>(),
            std::mem::size_of::<RIO_EXTENSION_FUNCTION_TABLE>() as u32,
            &mut bytes,
            ptr::null_mut::<OVERLAPPED>(),
            None,
        )
    };

    if rc != 0 {
        let err = io::Error::last_os_error();
        if is_rio_unavailable(err.raw_os_error()) {
            return Ok(None);
        }
        return Err(err);
    }

    if (bytes as usize) < std::mem::size_of::<RIO_EXTENSION_FUNCTION_TABLE>() {
        // Kernel wrote fewer bytes than the full table; treat as unavailable.
        return Ok(None);
    }

    // SAFETY: WSAIoctl reported success and wrote a full
    // RIO_EXTENSION_FUNCTION_TABLE; the buffer is initialised.
    #[allow(unsafe_code)]
    let table = unsafe { table.assume_init() };

    let register_buffer = unwrap_fn(table.RIORegisterBuffer)?;
    let deregister_buffer = unwrap_fn(table.RIODeregisterBuffer)?;
    let send = unwrap_fn(table.RIOSend)?;
    let receive = unwrap_fn(table.RIOReceive)?;
    let create_completion_queue = unwrap_fn(table.RIOCreateCompletionQueue)?;
    let close_completion_queue = unwrap_fn(table.RIOCloseCompletionQueue)?;
    let create_request_queue = unwrap_fn(table.RIOCreateRequestQueue)?;
    let dequeue_completion = unwrap_fn(table.RIODequeueCompletion)?;
    let notify = unwrap_fn(table.RIONotify)?;

    Ok(Some(RioFunctions {
        register_buffer,
        deregister_buffer,
        send,
        receive,
        create_completion_queue,
        close_completion_queue,
        create_request_queue,
        dequeue_completion,
        notify,
    }))
}

fn unwrap_fn<F>(opt: Option<F>) -> io::Result<F> {
    opt.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "RIO extension function table is missing a required entry",
        )
    })
}

/// Maps `WSAIoctl` failure codes to "RIO not present".
///
/// `WSAEINVAL` (10022) is returned when the kernel does not recognise the
/// extension GUID. `WSAEOPNOTSUPP` (10045) is returned when the Winsock
/// provider catalog does not register a RIO-capable transport. Anything else
/// is surfaced as an error so callers can investigate.
fn is_rio_unavailable(code: Option<i32>) -> bool {
    const WSAEINVAL: i32 = 10022;
    const WSAEOPNOTSUPP: i32 = 10045;
    match code {
        Some(c) => c == WSAEINVAL || c == WSAEOPNOTSUPP,
        None => false,
    }
}

/// Pre-allocated registered buffer block carved into fixed-size slots.
///
/// The block is registered with `RIORegisterBuffer` on construction and
/// deregistered via `RIODeregisterBuffer` on drop. Each [`RegisteredBuffer`]
/// handed out by [`Self::acquire`] is a `RIO_BUF` pointing at a distinct slot
/// within the block; callers pass the `RIO_BUF` to `RIOSend` / `RIOReceive`.
///
/// The pool is intentionally simple: an `Arc<RioBufferPoolInner>` so a clone
/// shares the registered block, plus an internal free list of slot indices.
/// The free list uses a `Mutex` because RIO submissions originate from any
/// thread; the contention is bounded by the slot count and never on the
/// critical send/recv path itself.
pub struct RioBufferPool {
    inner: Arc<RioBufferPoolInner>,
}

struct RioBufferPoolInner {
    // Block ownership: stored as a raw pointer + length pair so individual
    // slots can be carved out as `*mut u8` regions without violating Rust's
    // aliasing rules around shared `Arc`s. The block is allocated as a
    // `Box<[u8]>` and reclaimed in Drop via `Box::from_raw`. Slots are
    // disjoint byte ranges; ownership of each slot is conveyed by the
    // uniquely-held [`RegisteredBuffer`] handle.
    block_ptr: *mut u8,
    block_len: usize,
    buffer_id: RIO_BUFFERID,
    slot_size: u32,
    slot_count: u32,
    deregister: NonNullDeregisterBuffer,
    free_slots: std::sync::Mutex<Vec<u32>>,
}

// SAFETY: `RioBufferPoolInner` owns its registered block exclusively via a
// stable raw pointer / length pair reclaimed in Drop. The kernel side
// reads/writes only happen during outstanding RIO ops gated by uniquely-held
// [`RegisteredBuffer`] handles, so slot aliasing across threads is bounded by
// ownership. The deregister function pointer is `Copy`, the buffer id is an
// `isize`, the free-list is protected by a `Mutex`, and the raw pointer is
// only ever offset into - it is never freed except in Drop. These together
// satisfy both Send and Sync.
#[allow(unsafe_code)]
unsafe impl Send for RioBufferPoolInner {}
#[allow(unsafe_code)]
unsafe impl Sync for RioBufferPoolInner {}

impl RioBufferPool {
    /// Builds a pool with [`DEFAULT_RIO_POOL_BYTES`] total and
    /// [`DEFAULT_RIO_SLOT_BYTES`] per slot.
    ///
    /// # Errors
    ///
    /// Returns the underlying Winsock error when `RIORegisterBuffer` fails
    /// (typically `WSAENOBUFS` if the registered-buffer quota is exhausted,
    /// or `WSA_INVALID_HANDLE` when the RIO extension is not initialised on
    /// the host process).
    pub fn new(rio: &RioFunctions) -> io::Result<Self> {
        Self::with_capacity(rio, DEFAULT_RIO_POOL_BYTES, DEFAULT_RIO_SLOT_BYTES)
    }

    /// Builds a pool with a caller-supplied total and slot size.
    ///
    /// `total_bytes` is rounded down to an integer multiple of `slot_bytes`.
    /// Slot count is `total_bytes / slot_bytes`. Both arguments must be
    /// non-zero; otherwise an `InvalidInput` error is returned without
    /// touching the kernel.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for zero-sized arguments and the underlying
    /// Winsock error for `RIORegisterBuffer` failures.
    pub fn with_capacity(
        rio: &RioFunctions,
        total_bytes: usize,
        slot_bytes: usize,
    ) -> io::Result<Self> {
        if total_bytes == 0 || slot_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "RIO pool dimensions must be non-zero",
            ));
        }
        if slot_bytes > total_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "RIO slot size must not exceed total pool size",
            ));
        }

        let slot_count = (total_bytes / slot_bytes) as u32;
        let effective_bytes = (slot_count as usize) * slot_bytes;

        // Allocate as a boxed slice and convert to a raw pointer so disjoint
        // slot ranges can be exposed as `*mut u8` without aliasing through a
        // shared `Vec` reference held by the Arc.
        let block: Box<[u8]> = vec![0u8; effective_bytes].into_boxed_slice();
        let block_len = block.len();
        let block_ptr = Box::into_raw(block).cast::<u8>();

        // SAFETY: `block_ptr` points at a valid, owned allocation of
        // `block_len` bytes. The registered region is valid for exactly the
        // lifetime of the Arc-owned inner state because Drop reconstructs
        // the box from this pointer. RIORegisterBuffer returns
        // RIO_INVALID_BUFFERID on failure which we map to the last OS error.
        #[allow(unsafe_code)]
        let buffer_id = unsafe {
            (rio.register_buffer)(
                block_ptr as windows_sys::core::PCSTR,
                effective_bytes as u32,
            )
        };
        if buffer_id == RIO_INVALID_BUFFERID {
            let err = io::Error::last_os_error();
            // Reclaim the allocation since we are not handing ownership to
            // the kernel - RIORegisterBuffer failed before binding it.
            // SAFETY: `block_ptr` was just returned by Box::into_raw with
            // the same length and has not been deregistered yet (registration
            // failed).
            #[allow(unsafe_code)]
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    block_ptr, block_len,
                )));
            }
            return Err(err);
        }

        let free_slots = (0..slot_count).rev().collect::<Vec<_>>();

        Ok(Self {
            inner: Arc::new(RioBufferPoolInner {
                block_ptr,
                block_len,
                buffer_id,
                slot_size: slot_bytes as u32,
                slot_count,
                deregister: rio.deregister_buffer,
                free_slots: std::sync::Mutex::new(free_slots),
            }),
        })
    }

    /// Returns the configured slot size in bytes.
    #[must_use]
    pub fn slot_size(&self) -> u32 {
        self.inner.slot_size
    }

    /// Returns the total slot count.
    #[must_use]
    pub fn slot_count(&self) -> u32 {
        self.inner.slot_count
    }

    /// Returns the number of slots currently available.
    #[must_use]
    pub fn available_slots(&self) -> usize {
        self.inner
            .free_slots
            .lock()
            .map(|guard| guard.len())
            .unwrap_or(0)
    }

    /// Acquires a registered buffer slot.
    ///
    /// Returns `None` when the pool is exhausted. The caller pairs the
    /// returned descriptor with a `RIOSend` / `RIOReceive` request via
    /// [`RegisteredBuffer::as_rio_buf`]; on drop the slot is returned to the
    /// free list.
    #[must_use]
    pub fn acquire(&self) -> Option<RegisteredBuffer> {
        let slot = {
            let mut guard = self.inner.free_slots.lock().ok()?;
            guard.pop()?
        };
        Some(RegisteredBuffer {
            pool: Arc::clone(&self.inner),
            slot,
        })
    }
}

impl Drop for RioBufferPoolInner {
    fn drop(&mut self) {
        if self.buffer_id != RIO_INVALID_BUFFERID {
            // SAFETY: `buffer_id` was returned by RIORegisterBuffer and not
            // yet deregistered. Deregister before reclaiming the allocation
            // so the kernel cannot reference a freed page.
            #[allow(unsafe_code)]
            unsafe {
                (self.deregister)(self.buffer_id);
            }
        }
        if !self.block_ptr.is_null() {
            // SAFETY: `block_ptr` / `block_len` came from `Box::into_raw` on a
            // boxed slice and have not been reclaimed yet. Reconstructing the
            // box restores ownership so the allocation is freed correctly.
            #[allow(unsafe_code)]
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    self.block_ptr,
                    self.block_len,
                )));
            }
        }
    }
}

/// One acquired slot inside a [`RioBufferPool`].
///
/// Provides:
///
/// - [`Self::as_rio_buf`] - the `RIO_BUF` descriptor to pass to `RIOSend` /
///   `RIOReceive`.
/// - [`Self::as_slice`] / [`Self::as_mut_slice`] - safe access to the slot's
///   bytes for callers that want to fill a send buffer or inspect received
///   data after the completion fires.
///
/// On drop, the slot is returned to the pool's free list.
pub struct RegisteredBuffer {
    pool: Arc<RioBufferPoolInner>,
    slot: u32,
}

impl RegisteredBuffer {
    /// Builds the `RIO_BUF` descriptor for this slot.
    ///
    /// The `Offset` field is the byte offset of the slot within the
    /// registered block; `Length` is the full slot size. Callers that want
    /// to send fewer bytes pass a partial length via
    /// [`Self::as_rio_buf_with_len`].
    #[must_use]
    pub fn as_rio_buf(&self) -> RIO_BUF {
        RIO_BUF {
            BufferId: self.pool.buffer_id,
            Offset: self.slot * self.pool.slot_size,
            Length: self.pool.slot_size,
        }
    }

    /// Builds a `RIO_BUF` descriptor with a caller-supplied byte length.
    ///
    /// `length` is clamped to the slot size; passing a longer value returns
    /// a descriptor capped at the slot capacity. The dispatcher uses this
    /// form for partial sends (when the multiplex layer has fewer than
    /// `slot_size` bytes to push) and for the receive completion path
    /// (where the kernel reports the actual byte count).
    #[must_use]
    pub fn as_rio_buf_with_len(&self, length: u32) -> RIO_BUF {
        RIO_BUF {
            BufferId: self.pool.buffer_id,
            Offset: self.slot * self.pool.slot_size,
            Length: length.min(self.pool.slot_size),
        }
    }

    /// Read-only view of the slot's bytes.
    ///
    /// Returns a slice into the registered block at the slot's offset. The
    /// pool's free-list invariant guarantees the slot is uniquely held by
    /// this `RegisteredBuffer`, so the byte range is exclusively owned for
    /// the borrow's lifetime.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        let start = (self.slot as usize) * (self.pool.slot_size as usize);
        let len = self.pool.slot_size as usize;
        // SAFETY: `pool.block_ptr` points at a valid allocation of
        // `pool.block_len` bytes for the lifetime of the Arc-owned inner
        // state. `start + len` is bounded by `pool.block_len` because the
        // free-list only contains slot indices in `0..slot_count` and each
        // slot covers `slot_size` bytes. We hold a shared borrow of `self`,
        // matching the shared slice we hand back.
        #[allow(unsafe_code)]
        unsafe {
            std::slice::from_raw_parts(self.pool.block_ptr.add(start), len)
        }
    }

    /// Mutable view of the slot's bytes.
    ///
    /// Callers must not write to the slot while a `RIOSend` or `RIOReceive`
    /// referencing this slot is outstanding; the kernel reads/writes the
    /// registered memory directly. The dispatcher enforces this by handing
    /// out a `RegisteredBuffer` and not returning it to the pool until the
    /// corresponding completion has been observed. Since the slot is
    /// uniquely held while this method is callable (no `Clone` on
    /// `RegisteredBuffer`) the aliasing rule is upheld by ownership.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let start = (self.slot as usize) * (self.pool.slot_size as usize);
        let len = self.pool.slot_size as usize;
        // SAFETY: We hold `&mut self`, and slot indices are partitioned by
        // the free-list so no other `RegisteredBuffer` references the same
        // byte range. `pool.block_ptr` is valid for `pool.block_len` bytes
        // for the lifetime of the Arc-owned inner state; the bounds hold
        // because slot indices are confined to `0..slot_count`.
        #[allow(unsafe_code)]
        unsafe {
            std::slice::from_raw_parts_mut(self.pool.block_ptr.add(start), len)
        }
    }
}

impl Drop for RegisteredBuffer {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.pool.free_slots.lock() {
            guard.push(self.slot);
        }
    }
}

/// Wrapper around `RIOCreateCompletionQueue` / `RIOCloseCompletionQueue`.
///
/// Created with a caller-supplied queue depth; the kernel allocates an
/// internal ring of that size and returns a `RIO_CQ` handle. Drop closes the
/// queue automatically.
pub struct RioCompletionQueue {
    cq: RIO_CQ,
    close: NonNullCloseCompletionQueue,
    dequeue: NonNullDequeueCompletion,
}

// SAFETY: RIO_CQ is an opaque kernel handle (isize); thread safety of
// dispatching against it is the caller's responsibility (the dispatcher
// serialises dequeue calls). The function pointers are immutable Copy.
#[allow(unsafe_code)]
unsafe impl Send for RioCompletionQueue {}
#[allow(unsafe_code)]
unsafe impl Sync for RioCompletionQueue {}

impl RioCompletionQueue {
    /// Creates a queue with the given depth (number of completion entries).
    ///
    /// Depth must be in `1..=RIO_MAX_CQ_SIZE` (128 MiB entries on current
    /// Windows). The notification type is set to `RIO_IOCP_COMPLETION` so a
    /// downstream IOCP pump can drain completions; the IOCP plumbing is
    /// wired by the daemon dispatcher in a follow-up task.
    ///
    /// # Errors
    ///
    /// Returns the underlying Winsock error when the kernel refuses (most
    /// commonly `WSAENOBUFS` when depth exceeds the registered-queue quota).
    pub fn new(rio: &RioFunctions, depth: u32) -> io::Result<Self> {
        if depth == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "RIO completion queue depth must be non-zero",
            ));
        }
        // Use the polling-style completion (no IOCP, no event handle). The
        // dispatcher polls with RIODequeueCompletion at a cadence driven by
        // the existing CompletionPump. The completion-type field is the
        // last 4 bytes of the union; passing a null/None notification means
        // "poll-only".
        // SAFETY: We pass a null RIO_NOTIFICATION_COMPLETION pointer which
        // selects the poll-only mode per the Win32 docs. RIO_INVALID_CQ is
        // returned on failure.
        #[allow(unsafe_code)]
        let cq = unsafe { (rio.create_completion_queue)(depth, ptr::null()) };
        if cq == RIO_INVALID_CQ {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            cq,
            close: rio.close_completion_queue,
            dequeue: rio.dequeue_completion,
        })
    }

    /// Drains up to `out.len()` completions from the queue.
    ///
    /// Returns the number of `RIORESULT` entries the kernel wrote into
    /// `out`. A return of `0` means the queue is empty; callers loop until
    /// they observe enough completions or rely on `RIONotify` (wired in a
    /// later task) for sleep/wake.
    pub fn dequeue(&self, out: &mut [windows_sys::Win32::Networking::WinSock::RIORESULT]) -> usize {
        if out.is_empty() {
            return 0;
        }
        // SAFETY: `out` is a caller-owned slice of RIORESULTs; the kernel
        // writes up to `arraysize` entries. `self.cq` is valid until Drop.
        #[allow(unsafe_code)]
        let n = unsafe { (self.dequeue)(self.cq, out.as_mut_ptr(), out.len() as u32) };
        n as usize
    }

    /// Returns the raw `RIO_CQ` handle.
    ///
    /// Useful when wiring a per-socket request queue
    /// (`RIOCreateRequestQueue`) that needs to reference both the send and
    /// receive completion queues. The handle remains owned by this
    /// `RioCompletionQueue`; the caller must not close it.
    #[must_use]
    pub fn raw(&self) -> RIO_CQ {
        self.cq
    }
}

impl Drop for RioCompletionQueue {
    fn drop(&mut self) {
        if self.cq != RIO_INVALID_CQ {
            // SAFETY: `cq` was returned by RIOCreateCompletionQueue and not
            // yet closed.
            #[allow(unsafe_code)]
            unsafe {
                (self.close)(self.cq);
            }
        }
    }
}

/// Submits a `RIOSend` request against the supplied request queue.
///
/// `flags` defaults to `0` for normal commit-and-notify semantics; callers
/// that want batched submission (`RIO_MSG_DEFER`) pass it explicitly. The
/// `request_context` opaque value is round-tripped through to the matching
/// `RIORESULT::RequestContext` field, which the dispatcher uses to correlate
/// completions with their originating slot.
///
/// # Errors
///
/// Returns `io::Error::last_os_error()` when `RIOSend` returns `FALSE`.
pub fn rio_send(
    rio: &RioFunctions,
    rq: RIO_RQ,
    buffer: &RIO_BUF,
    flags: u32,
    request_context: usize,
) -> io::Result<()> {
    // SAFETY: `rq` is a caller-supplied RIO_RQ handle; the buffer descriptor
    // refers to a slot inside an outstanding RioBufferPool which is kept
    // alive by the caller for the duration of the submission. The
    // `request_context` is treated by the kernel as an opaque void* with no
    // validity requirement beyond fitting in `*const c_void`.
    #[allow(unsafe_code)]
    let ok = unsafe {
        (rio.send)(
            rq,
            buffer as *const RIO_BUF,
            1,
            flags,
            request_context as *const c_void,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Submits a `RIOReceive` request against the supplied request queue.
///
/// See [`rio_send`] for the meaning of the `flags` and `request_context`
/// arguments.
///
/// # Errors
///
/// Returns `io::Error::last_os_error()` when `RIOReceive` returns `FALSE`.
pub fn rio_recv(
    rio: &RioFunctions,
    rq: RIO_RQ,
    buffer: &RIO_BUF,
    flags: u32,
    request_context: usize,
) -> io::Result<()> {
    // SAFETY: same argument as `rio_send`; the kernel writes into the
    // registered buffer region pointed at by `buffer` on completion.
    #[allow(unsafe_code)]
    let ok = unsafe {
        (rio.receive)(
            rq,
            buffer as *const RIO_BUF,
            1,
            flags,
            request_context as *const c_void,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Creates a registered request queue for an existing socket.
///
/// `max_outstanding_recv` and `max_outstanding_send` bound how many
/// `RIOReceive` and `RIOSend` requests may be in flight simultaneously on
/// the socket. `recv_cq` and `send_cq` are the completion queues that will
/// receive the corresponding `RIORESULT` entries; the dispatcher typically
/// uses one shared CQ for many sockets.
///
/// # Errors
///
/// Returns the underlying Winsock error when `RIOCreateRequestQueue` fails.
/// The most common cause is the socket having been created without
/// `WSA_FLAG_REGISTERED_IO`.
pub fn rio_create_request_queue(
    rio: &RioFunctions,
    socket: RawSocket,
    max_outstanding_recv: u32,
    max_outstanding_send: u32,
    recv_cq: &RioCompletionQueue,
    send_cq: &RioCompletionQueue,
) -> io::Result<RIO_RQ> {
    // SAFETY: `socket` is a caller-supplied raw socket handle; the
    // completion queues are owned by the caller and outlive the RIO_RQ
    // because the caller keeps them alive until the socket is closed.
    #[allow(unsafe_code)]
    let rq = unsafe {
        (rio.create_request_queue)(
            socket as SOCKET,
            max_outstanding_recv,
            // 1 RIO_BUF per RIOReceive submission - the dispatcher passes
            // a single buffer per call.
            1,
            max_outstanding_send,
            1,
            recv_cq.cq,
            send_cq.cq,
            ptr::null(),
        )
    };
    if rq == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(rq)
}

/// Re-arms RIO notifications on the given completion queue.
///
/// Only meaningful when the queue was created with a notification type
/// (`RIO_EVENT_COMPLETION` or `RIO_IOCP_COMPLETION`). The polling-mode
/// queues built by [`RioCompletionQueue::new`] do not need to be re-armed,
/// but the helper is exposed for future dispatchers that switch to
/// event-driven notifications.
///
/// # Errors
///
/// Returns `io::Error::last_os_error()` when `RIONotify` returns non-zero.
pub fn rio_notify(rio: &RioFunctions, cq: &RioCompletionQueue) -> io::Result<()> {
    // SAFETY: `cq.cq` is a valid RIO_CQ owned by the supplied wrapper.
    #[allow(unsafe_code)]
    let rc = unsafe { (rio.notify)(cq.cq) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_default_is_off() {
        assert_eq!(parse_rio_env(None), RioMode::Off);
        assert_eq!(parse_rio_env(Some("")), RioMode::Off);
        assert_eq!(parse_rio_env(Some("off")), RioMode::Off);
    }

    #[test]
    fn env_var_parses_auto_case_insensitive() {
        assert_eq!(parse_rio_env(Some("auto")), RioMode::Auto);
        assert_eq!(parse_rio_env(Some("AUTO")), RioMode::Auto);
        assert_eq!(parse_rio_env(Some("Auto")), RioMode::Auto);
        assert_eq!(parse_rio_env(Some("aUtO")), RioMode::Auto);
    }

    #[test]
    fn env_var_parses_on_case_insensitive() {
        assert_eq!(parse_rio_env(Some("on")), RioMode::On);
        assert_eq!(parse_rio_env(Some("ON")), RioMode::On);
        assert_eq!(parse_rio_env(Some("On")), RioMode::On);
    }

    #[test]
    fn env_var_trims_whitespace() {
        assert_eq!(parse_rio_env(Some("  auto  ")), RioMode::Auto);
        assert_eq!(parse_rio_env(Some("\ton\n")), RioMode::On);
    }

    #[test]
    fn env_var_unknown_values_fall_back_to_off() {
        assert_eq!(parse_rio_env(Some("yes")), RioMode::Off);
        assert_eq!(parse_rio_env(Some("1")), RioMode::Off);
        assert_eq!(parse_rio_env(Some("true")), RioMode::Off);
        assert_eq!(parse_rio_env(Some("disabled")), RioMode::Off);
    }

    #[test]
    fn default_pool_dimensions_are_sane() {
        assert!(DEFAULT_RIO_POOL_BYTES >= DEFAULT_RIO_SLOT_BYTES);
        assert_eq!(DEFAULT_RIO_POOL_BYTES % DEFAULT_RIO_SLOT_BYTES, 0);
        let slot_count = DEFAULT_RIO_POOL_BYTES / DEFAULT_RIO_SLOT_BYTES;
        assert!(slot_count >= 2, "pool must provide at least 2 slots");
    }

    #[test]
    fn rio_unavailable_codes_recognised() {
        assert!(is_rio_unavailable(Some(10022))); // WSAEINVAL
        assert!(is_rio_unavailable(Some(10045))); // WSAEOPNOTSUPP
        assert!(!is_rio_unavailable(Some(10054))); // WSAECONNRESET
        assert!(!is_rio_unavailable(None));
    }

    #[test]
    fn rio_mode_default_is_off() {
        assert_eq!(RioMode::default(), RioMode::Off);
    }

    #[test]
    fn rio_mode_debug_contains_variant_name() {
        assert!(format!("{:?}", RioMode::Off).contains("Off"));
        assert!(format!("{:?}", RioMode::On).contains("On"));
        assert!(format!("{:?}", RioMode::Auto).contains("Auto"));
    }
}
