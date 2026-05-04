//! Completion-port pump for batched IOCP operations.
//!
//! A [`CompletionPump`] owns a shared [`CompletionPort`] and a worker thread
//! that drains the port via `GetQueuedCompletionStatusEx` in batches. Each
//! in-flight overlapped operation registers a [`CompletionHandler`] keyed by
//! the address of its `OVERLAPPED` structure; the pump dequeues completion
//! packets and dispatches them to the matching handler.
//!
//! This is the Windows analogue of an io_uring completion-queue drain. It
//! decouples submission of overlapped writes/reads from waiting for them so
//! callers can submit a batch and continue useful work while the pump fans
//! completions out to per-operation handlers (channels, wakers, callbacks).
//!
//! # Architecture
//!
//! ```text
//!     submitter thread                pump thread
//!     ----------------                -----------
//!     register(overlapped, handler)
//!     WriteFile(... overlapped ...)
//!                                     GetQueuedCompletionStatusEx (batch)
//!                                     -> for each entry:
//!                                          look up handler by overlapped ptr
//!                                          handler(Ok(transferred))
//! ```
//!
//! # Shutdown
//!
//! [`CompletionPump::shutdown`] posts a sentinel completion via
//! `PostQueuedCompletionStatus` with a reserved completion key; the worker
//! observes the sentinel, drains any remaining real completions, and exits.
//! [`Drop`] performs the same shutdown if the caller did not call it.
//!
//! # Upstream reference
//!
//! Upstream rsync 3.4.1 (`target/interop/upstream-src/rsync-3.4.1/`) does
//! not use IOCP; the design space for the pump surface is open. The pump
//! is intentionally minimal so #1898 can layer a batched-write API on top
//! that mirrors `IoUringDiskBatch` (`crates/fast_io/src/io_uring/disk_batch.rs`).
//!
//! # Cross-platform
//!
//! Real implementation lives behind `#[cfg(all(target_os = "windows",
//! feature = "iocp"))]`. The non-Windows stub (`crate::iocp_stub`) provides
//! the same public types with `Unsupported` errors so the crate compiles on
//! Linux and macOS.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::{
    ERROR_ABANDONED_WAIT_0, ERROR_HANDLE_EOF, ERROR_INSUFFICIENT_BUFFER, FALSE, HANDLE, TRUE,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::IO::{
    GetQueuedCompletionStatusEx, OVERLAPPED, OVERLAPPED_ENTRY, PostQueuedCompletionStatus,
};

use super::completion_port::CompletionPort;
use super::error::classify_overlapped_error;

/// Reserved completion key used to signal pump shutdown.
///
/// Any valid file-handle association uses non-sentinel keys. Picking
/// `usize::MAX` keeps the value distinct from typical small key ids assigned
/// by reader/writer constructors.
const SHUTDOWN_KEY: usize = usize::MAX;

/// Maximum number of completion entries dequeued per
/// `GetQueuedCompletionStatusEx` call.
///
/// Larger batches amortize the syscall cost across more completions but also
/// increase per-call latency. 64 matches the io_uring CQE batch sizing used
/// by `crates/fast_io/src/io_uring/disk_batch.rs`.
const DEFAULT_BATCH_SIZE: usize = 64;

/// Hard cap on dynamic batch growth when the drain loop encounters
/// [`ERROR_INSUFFICIENT_BUFFER`] (issue #1930).
///
/// The drain buffer doubles in size each time the kernel signals that more
/// completions are available than fit in the current array, up to this cap.
/// 8192 entries at `sizeof(OVERLAPPED_ENTRY) == 32` bytes is 256 KiB - a
/// reasonable upper bound that prevents pathological cases (a buggy producer
/// flooding the port) from exhausting memory.
const MAX_BATCH_SIZE: usize = 8192;

/// Wait timeout for a single drain call, in milliseconds.
///
/// The pump uses a finite (non-INFINITE) timeout so the worker thread can
/// observe `running == false` even if no operations are in flight, providing
/// a fail-safe when a caller drops the pump without calling `shutdown`.
/// The shutdown sentinel still wakes the thread immediately in the common
/// case; the timeout only matters when the sentinel post somehow fails.
const DRAIN_TIMEOUT_MS: u32 = 100;

/// Boxed callback dispatched when an overlapped operation completes.
///
/// The argument is the result of the I/O: `Ok(bytes_transferred)` on success,
/// or `Err` carrying the OS error reported through the completion entry.
///
/// Handlers run on the pump worker thread and must not block; long-running
/// work should be forwarded to a channel or other queue.
pub type CompletionHandler = Box<dyn FnOnce(io::Result<u32>) + Send + 'static>;

/// Configuration for a [`CompletionPump`].
#[derive(Debug, Clone)]
pub struct IocpPumpConfig {
    /// Maximum concurrent worker threads the OS allows on the port.
    ///
    /// Passed to `CreateIoCompletionPort` as `NumberOfConcurrentThreads`.
    /// `0` lets the OS pick (one per logical processor).
    pub max_concurrent_threads: u32,
    /// Maximum entries pulled in a single `GetQueuedCompletionStatusEx` call.
    pub batch_size: usize,
}

impl Default for IocpPumpConfig {
    fn default() -> Self {
        Self {
            max_concurrent_threads: 1,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

/// Shared registry of pending overlapped operations.
///
/// Keyed by the address of the `OVERLAPPED` structure (cast to `usize`),
/// which the kernel returns verbatim through each `OVERLAPPED_ENTRY`.
type HandlerRegistry = Mutex<HashMap<usize, CompletionHandler>>;

/// Inner state shared between the pump owner and the drain thread.
struct PumpInner {
    port: CompletionPort,
    handlers: HandlerRegistry,
    running: AtomicBool,
    config: IocpPumpConfig,
}

/// IOCP completion-port pump.
///
/// Owns a [`CompletionPort`] plus a worker thread driving
/// `GetQueuedCompletionStatusEx`. File handles are associated via
/// [`associate_handle`](Self::associate_handle); per-operation completion
/// handlers are registered via [`register`](Self::register) before the
/// `WriteFile` / `ReadFile` call that produces the matching completion.
///
/// # Lifetime
///
/// The pump holds `Arc<PumpInner>` shared with the worker thread. Dropping
/// the pump triggers a shutdown post and joins the worker. Calling
/// [`shutdown`](Self::shutdown) explicitly is recommended so callers see
/// any I/O errors raised by the worker on its way out.
///
/// # Thread safety
///
/// `CompletionPump` is `Send + Sync`: the underlying completion port is a
/// kernel object designed for concurrent access, and the handler registry is
/// behind a `Mutex`. Multiple submitter threads can register handlers and
/// post submissions concurrently against a single pump.
pub struct CompletionPump {
    inner: Arc<PumpInner>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl std::fmt::Debug for CompletionPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletionPump")
            .field("running", &self.is_running())
            .field("pending_ops", &self.pending_ops())
            .field("config", &self.inner.config)
            .finish()
    }
}

impl CompletionPump {
    /// Creates a new pump with default configuration.
    pub fn new() -> io::Result<Self> {
        Self::with_config(IocpPumpConfig::default())
    }

    /// Creates a new pump with the given configuration.
    ///
    /// Spawns the drain worker thread immediately. Returns an error if the
    /// completion port cannot be created or the worker cannot be spawned.
    pub fn with_config(config: IocpPumpConfig) -> io::Result<Self> {
        let port = CompletionPort::new(config.max_concurrent_threads)?;
        let inner = Arc::new(PumpInner {
            port,
            handlers: Mutex::new(HashMap::new()),
            running: AtomicBool::new(true),
            config,
        });

        let worker_inner = Arc::clone(&inner);
        let worker = thread::Builder::new()
            .name("iocp-pump".into())
            .spawn(move || drain_loop(worker_inner))?;

        Ok(Self {
            inner,
            worker: Some(worker),
        })
    }

    /// Returns the raw completion-port handle.
    ///
    /// Callers pass this handle to `CreateIoCompletionPort` (via
    /// [`associate_handle`](Self::associate_handle)) or to a manual
    /// `GetQueuedCompletionStatus` call when bypassing the pump for a
    /// synchronous wait.
    #[must_use]
    pub fn port_handle(&self) -> HANDLE {
        self.inner.port.handle()
    }

    /// Associates a file (or socket) handle with the pump's completion port.
    ///
    /// `key` is returned with each completion entry from the worker; it
    /// identifies the originating handle when the pump is shared across many
    /// files. Reserved key [`SHUTDOWN_KEY`] (`usize::MAX`) must not be used.
    pub fn associate_handle(&self, file_handle: HANDLE, key: usize) -> io::Result<()> {
        if key == SHUTDOWN_KEY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "completion key usize::MAX is reserved for pump shutdown signalling",
            ));
        }
        self.inner.port.associate(file_handle, key)
    }

    /// Registers a completion handler for an overlapped operation.
    ///
    /// `overlapped_ptr` must point to the same `OVERLAPPED` structure that
    /// will be passed to the subsequent `WriteFile` / `ReadFile` call. The
    /// registry uses the pointer address as the lookup key, so each in-flight
    /// operation must use a distinct, stable `OVERLAPPED` (typically a pinned
    /// allocation).
    ///
    /// The handler is invoked exactly once on the pump worker thread when
    /// the operation completes. If submission of the operation itself fails
    /// (i.e. `WriteFile` returns false with an error other than
    /// `ERROR_IO_PENDING`), callers must call [`unregister`](Self::unregister)
    /// to drop the handler without invocation.
    pub fn register(&self, overlapped_ptr: *mut OVERLAPPED, handler: CompletionHandler) {
        let mut handlers = self
            .inner
            .handlers
            .lock()
            .expect("iocp pump handler registry poisoned");
        handlers.insert(overlapped_ptr as usize, handler);
    }

    /// Removes a previously registered handler without invoking it.
    ///
    /// Use this when the I/O submission fails synchronously and no completion
    /// will ever arrive. Returns `true` if a handler was removed.
    pub fn unregister(&self, overlapped_ptr: *mut OVERLAPPED) -> bool {
        let mut handlers = self
            .inner
            .handlers
            .lock()
            .expect("iocp pump handler registry poisoned");
        handlers.remove(&(overlapped_ptr as usize)).is_some()
    }

    /// Returns the number of in-flight operations awaiting completion.
    #[must_use]
    pub fn pending_ops(&self) -> usize {
        self.inner
            .handlers
            .lock()
            .expect("iocp pump handler registry poisoned")
            .len()
    }

    /// Returns whether the pump worker is still running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Acquire)
    }

    /// Shuts the pump down and joins the worker thread.
    ///
    /// Posts a sentinel completion via `PostQueuedCompletionStatus`, the
    /// worker drains any remaining real completions, dispatches them, and
    /// exits. Any handlers still registered after the worker exits are
    /// dropped without being invoked - their operations either never
    /// completed or are about to fail because the port is closing.
    pub fn shutdown(mut self) -> io::Result<()> {
        self.shutdown_impl()
    }

    fn shutdown_impl(&mut self) -> io::Result<()> {
        // Idempotent: only the first caller flips `running` to false.
        if !self.inner.running.swap(false, Ordering::AcqRel) {
            // Worker may already be joined.
            if let Some(handle) = self.worker.take() {
                return handle.join().expect("iocp pump worker panicked");
            }
            return Ok(());
        }

        // Wake the worker so it observes `running == false` immediately. Any
        // failure to post is non-fatal because the worker also wakes from its
        // bounded `GetQueuedCompletionStatusEx` timeout.
        // SAFETY: `inner.port` outlives this call; the sentinel uses no
        // OVERLAPPED pointer (NULL is the documented "completion key only"
        // form per Microsoft Learn `PostQueuedCompletionStatus`).
        #[allow(unsafe_code)]
        unsafe {
            PostQueuedCompletionStatus(
                self.inner.port.handle(),
                0,
                SHUTDOWN_KEY,
                std::ptr::null_mut(),
            );
        }

        if let Some(handle) = self.worker.take() {
            return handle.join().expect("iocp pump worker panicked");
        }
        Ok(())
    }
}

impl Drop for CompletionPump {
    fn drop(&mut self) {
        let _ = self.shutdown_impl();
    }
}

// SAFETY: PumpInner contains only Send + Sync state - CompletionPort is
// already Send + Sync (kernel object), AtomicBool is Sync, Mutex<HashMap<...>>
// is Sync because every CompletionHandler captured inside is `Send + 'static`
// per the type alias. The Arc itself is Send + Sync when its T is Sync.
//
// Manual impls are not needed because Arc<PumpInner> is auto-Send/Sync.

fn drain_loop(inner: Arc<PumpInner>) -> io::Result<()> {
    let initial_batch_size = inner.config.batch_size.max(1).min(MAX_BATCH_SIZE);
    // The batch buffer can grow at runtime if the kernel reports
    // ERROR_INSUFFICIENT_BUFFER (issue #1930). It never shrinks; a single
    // burst of completions warrants keeping the larger buffer alive for the
    // remainder of the pump's lifetime.
    let mut entries: Vec<OVERLAPPED_ENTRY> = vec![zeroed_entry(); initial_batch_size];

    while inner.running.load(Ordering::Acquire) {
        let mut removed: u32 = 0;

        // SAFETY: `inner.port` outlives the call (held via Arc); `entries`
        // backing storage is valid for `entries.len()` elements; `removed` is
        // a stack-local u32. Documentation:
        // https://learn.microsoft.com/windows/win32/api/ioapiset/nf-ioapiset-getqueuedcompletionstatusex
        #[allow(unsafe_code)]
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                inner.port.handle(),
                entries.as_mut_ptr(),
                entries.len() as u32,
                &mut removed,
                DRAIN_TIMEOUT_MS,
                FALSE,
            )
        };

        if ok == FALSE {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                // Bounded-timeout wake with no completions: loop and re-check
                // the running flag.
                Some(c) if c as u32 == WAIT_TIMEOUT => continue,
                // The port handle was closed while we were waiting; treat as
                // graceful shutdown.
                Some(c) if c as u32 == ERROR_ABANDONED_WAIT_0 => break,
                // Issue #1930: the kernel signalled that more completions
                // were available than our array could hold. Double the buffer
                // (capped at MAX_BATCH_SIZE) and retry on the next iteration
                // - completions remain queued on the port until drained.
                Some(c) if c as u32 == ERROR_INSUFFICIENT_BUFFER => {
                    if entries.len() < MAX_BATCH_SIZE {
                        let new_size = (entries.len().saturating_mul(2)).min(MAX_BATCH_SIZE);
                        entries.resize(new_size, zeroed_entry());
                        continue;
                    }
                    // Already at the cap - propagate the typed error so the
                    // pump owner can diagnose a runaway producer.
                    return Err(super::error::IocpError::InsufficientBuffer {
                        requested: 0,
                        capacity: entries.len() as u32,
                    }
                    .into());
                }
                // Upgrade ERROR_INVALID_PARAMETER to a typed error pointing
                // at the most likely cause - a handle associated with the
                // pump that was not opened with FILE_FLAG_OVERLAPPED.
                _ => {
                    return Err(classify_overlapped_error(
                        err,
                        "GetQueuedCompletionStatusEx",
                    ));
                }
            }
        }

        for entry in entries.iter().take(removed as usize) {
            // The shutdown sentinel carries no overlapped pointer.
            if entry.lpCompletionKey == SHUTDOWN_KEY {
                inner.running.store(false, Ordering::Release);
                continue;
            }

            let overlapped_ptr = entry.lpOverlapped;
            if overlapped_ptr.is_null() {
                // Spurious or user-posted notification with no associated op.
                continue;
            }

            let handler = {
                let mut handlers = inner
                    .handlers
                    .lock()
                    .expect("iocp pump handler registry poisoned");
                handlers.remove(&(overlapped_ptr as usize))
            };

            if let Some(handler) = handler {
                let result = completion_result(entry);
                handler(result);
            }
            // Otherwise: an OVERLAPPED arrived without a matching handler
            // (possible when callers submit raw I/O against the port without
            // registering a handler first). Drop silently - the submitter
            // owns responsibility for their own completions in that case.
        }
    }

    Ok(())
}

/// Interprets a single completion entry into an I/O result.
fn completion_result(entry: &OVERLAPPED_ENTRY) -> io::Result<u32> {
    // The internal status of the OVERLAPPED carries the NTSTATUS for the op.
    // Successful ops set `Internal == 0`; failures encode an NTSTATUS that
    // `RtlNtStatusToDosError` would translate. We extract the dwError via the
    // public Win32 surface using the entry's transferred byte count plus the
    // OVERLAPPED's `Internal` value: per
    // https://learn.microsoft.com/windows/win32/api/minwinbase/ns-minwinbase-overlapped
    // a non-zero `Internal` indicates failure.
    //
    // SAFETY: `entry.lpOverlapped` was returned by the kernel and remains
    // valid until the registered handler observes it.
    #[allow(unsafe_code)]
    let internal = unsafe { (*entry.lpOverlapped).Internal };

    // STATUS_END_OF_FILE manifests as ERROR_HANDLE_EOF (38) at the Win32
    // boundary; report it as `UnexpectedEof` rather than a generic error.
    if internal != 0 {
        let dos_error = ntstatus_to_dos_error(internal as u32);
        if dos_error == ERROR_HANDLE_EOF {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        return Err(io::Error::from_raw_os_error(dos_error as i32));
    }

    Ok(entry.dwNumberOfBytesTransferred)
}

/// Translates an NTSTATUS to its corresponding Win32 DOS error code.
///
/// IOCP delivers NTSTATUS in `OVERLAPPED.Internal`; the upper layer expects
/// classic Win32 errors (e.g., `ERROR_OPERATION_ABORTED` 995). Rather than
/// pull in `ntdll!RtlNtStatusToDosError`, we recognise the small set of
/// statuses that overlapped file I/O can produce and pass everything else
/// through unchanged so callers still get a meaningful `io::Error`.
fn ntstatus_to_dos_error(status: u32) -> u32 {
    // Selected mappings from
    // https://learn.microsoft.com/openspecs/windows_protocols/ms-erref/596a1078-e883-4972-9bbc-49e60bebca55
    // Only the subset that overlapped file I/O can return is included; any
    // unrecognised status is returned verbatim and surfaces as
    // `io::Error::from_raw_os_error` with a "Windows OS error code <n>" label.
    match status {
        0xC000_0011 => ERROR_HANDLE_EOF, // STATUS_END_OF_FILE
        0xC000_0120 => 995,              // STATUS_CANCELLED -> ERROR_OPERATION_ABORTED
        0xC000_009A => 1450,             // STATUS_INSUFFICIENT_RESOURCES
        0xC000_00B5 => 121,              // STATUS_IO_TIMEOUT -> ERROR_SEM_TIMEOUT
        other => other,
    }
}

fn zeroed_entry() -> OVERLAPPED_ENTRY {
    // SAFETY: OVERLAPPED_ENTRY is plain old data and is valid when zeroed.
    #[allow(unsafe_code)]
    unsafe {
        std::mem::zeroed()
    }
}

/// Posts a manual completion event to the pump.
///
/// Used primarily by tests and by future batched-write APIs (#1898) that
/// need to inject a synthetic completion (e.g. for cancellation). The
/// `bytes_transferred`, `key`, and `overlapped_ptr` are passed through to
/// the worker exactly as the kernel would deliver them.
///
/// # Safety considerations
///
/// `overlapped_ptr` must either be `null` (in which case the worker drops
/// the entry) or point to an `OVERLAPPED` whose handler is registered with
/// the pump. Posting an unregistered non-null overlapped is benign but
/// wastes a wakeup.
pub fn post_completion(
    pump: &CompletionPump,
    bytes_transferred: u32,
    key: usize,
    overlapped_ptr: *mut OVERLAPPED,
) -> io::Result<()> {
    // SAFETY: `pump` keeps the underlying port alive for the duration of
    // this call.
    #[allow(unsafe_code)]
    let ok = unsafe {
        PostQueuedCompletionStatus(pump.port_handle(), bytes_transferred, key, overlapped_ptr)
    };
    if ok == TRUE {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Convenience wrapper for callers that want to wait synchronously for one
/// completion via a channel-based handler.
///
/// Returns a registered handler plus the receiver; the handler can be
/// passed to [`CompletionPump::register`], and the receiver blocks until
/// the worker fires it.
#[must_use]
pub fn oneshot_handler() -> (
    CompletionHandler,
    std::sync::mpsc::Receiver<io::Result<u32>>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handler: CompletionHandler = Box::new(move |result| {
        // Send is best-effort: if the receiver has been dropped the caller
        // no longer cares about the result.
        let _ = tx.send(result);
    });
    (handler, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED,
        FILE_GENERIC_WRITE, FILE_SHARE_READ, WriteFile,
    };

    use super::super::overlapped::OverlappedOp;

    fn to_wide(path: &std::path::Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    #[test]
    fn pump_creates_and_shuts_down() {
        let pump = CompletionPump::new().expect("pump must construct");
        assert!(pump.is_running());
        assert_eq!(pump.pending_ops(), 0);
        pump.shutdown().expect("shutdown must succeed");
    }

    #[test]
    fn pump_rejects_reserved_key() {
        let pump = CompletionPump::new().unwrap();
        let dummy_handle = std::ptr::null_mut::<std::ffi::c_void>() as HANDLE;
        let err = pump
            .associate_handle(dummy_handle, SHUTDOWN_KEY)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn pump_unregister_returns_handler_present() {
        let pump = CompletionPump::new().unwrap();
        let mut op = OverlappedOp::new_write(0, b"abc");
        let ptr = op.as_overlapped_ptr();
        let (handler, _rx) = oneshot_handler();
        pump.register(ptr, handler);
        assert_eq!(pump.pending_ops(), 1);
        assert!(pump.unregister(ptr));
        assert_eq!(pump.pending_ops(), 0);
        assert!(!pump.unregister(ptr));
    }

    #[test]
    fn pump_dispatches_overlapped_write_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pumped_write.bin");
        let wide = to_wide(&path);

        // SAFETY: `wide` is a properly null-terminated wide string; flags
        // mirror those used by IocpWriter::create.
        #[allow(unsafe_code)]
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_GENERIC_WRITE,
                FILE_SHARE_READ,
                std::ptr::null(),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            handle,
            windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE,
            "CreateFileW failed: {}",
            io::Error::last_os_error()
        );

        let pump = CompletionPump::new().unwrap();
        pump.associate_handle(handle, 1).unwrap();

        let payload = b"pumped iocp write";
        let mut op = OverlappedOp::new_write(0, payload);
        let overlapped_ptr = op.as_overlapped_ptr();
        let (handler, rx) = oneshot_handler();
        pump.register(overlapped_ptr, handler);

        // SAFETY: `op` outlives the WriteFile call and the completion delivery
        // because we hold it on the stack until after `rx.recv_timeout`. The
        // pump dispatches the handler synchronously from its worker.
        #[allow(unsafe_code)]
        let success = unsafe {
            WriteFile(
                handle,
                op.buffer.as_ptr().cast(),
                payload.len() as u32,
                std::ptr::null_mut(),
                overlapped_ptr,
            )
        };
        if success != TRUE {
            let err = io::Error::last_os_error();
            assert_eq!(
                err.raw_os_error(),
                Some(997),
                "WriteFile must either succeed or return ERROR_IO_PENDING, got {err}"
            );
        }

        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("pump must dispatch completion within timeout")
            .expect("write must complete successfully");
        assert_eq!(result as usize, payload.len());

        // Drop the operation only after completion is observed.
        drop(op);

        // SAFETY: the handle was created by CreateFileW above and has not
        // been closed; CloseHandle releases it.
        #[allow(unsafe_code)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, payload);

        pump.shutdown().unwrap();
    }

    #[test]
    fn pump_post_completion_round_trip() {
        let pump = CompletionPump::new().unwrap();

        // Use a stable allocation as a fake OVERLAPPED address. The pump's
        // dispatch only inspects the address - the OVERLAPPED itself is not
        // dereferenced because we set Internal/InternalHigh to zero through
        // the stand-in entry below.
        let fake_overlapped: Box<OVERLAPPED> = Box::new(zeroed_overlapped());
        let raw_ptr = Box::into_raw(fake_overlapped);

        let (handler, rx) = oneshot_handler();
        pump.register(raw_ptr, handler);

        post_completion(&pump, 42, 7, raw_ptr).unwrap();

        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("post_completion must dispatch")
            .expect("manually posted entry must report success");
        assert_eq!(result, 42);

        // SAFETY: raw_ptr came from Box::into_raw and the handler has fired,
        // so no further completion is in flight referencing this pointer.
        #[allow(unsafe_code)]
        unsafe {
            drop(Box::from_raw(raw_ptr));
        }

        pump.shutdown().unwrap();
    }

    fn zeroed_overlapped() -> OVERLAPPED {
        // SAFETY: OVERLAPPED is plain-old-data and valid when zeroed.
        #[allow(unsafe_code)]
        unsafe {
            std::mem::zeroed()
        }
    }

    /// Issue #1930: posting more completions than the initial batch size must
    /// be handled by the drain loop without losing any. With the dynamic
    /// growth introduced for ERROR_INSUFFICIENT_BUFFER, the kernel can never
    /// surface that error to user code at the default batch size; verifying
    /// that no completion is dropped exercises the same code path.
    #[test]
    fn pump_drains_burst_larger_than_batch_size() {
        let pump = CompletionPump::new().unwrap();
        let burst = DEFAULT_BATCH_SIZE * 4;

        let mut allocations: Vec<*mut OVERLAPPED> = Vec::with_capacity(burst);
        let mut receivers = Vec::with_capacity(burst);

        for _ in 0..burst {
            let fake: Box<OVERLAPPED> = Box::new(zeroed_overlapped());
            let raw = Box::into_raw(fake);
            let (handler, rx) = oneshot_handler();
            pump.register(raw, handler);
            allocations.push(raw);
            receivers.push(rx);
            post_completion(&pump, 1, 5, raw).unwrap();
        }

        for rx in receivers {
            let value = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("every burst entry must dispatch")
                .expect("entry must report success");
            assert_eq!(value, 1);
        }

        for raw in allocations {
            // SAFETY: raw came from Box::into_raw above and every handler has
            // fired, so no completion still references this pointer.
            #[allow(unsafe_code)]
            unsafe {
                drop(Box::from_raw(raw));
            }
        }

        pump.shutdown().unwrap();
    }

    #[test]
    fn iocp_error_insufficient_buffer_round_trips() {
        // The typed error mapping is exercised here independently of the
        // pump because reproducing ERROR_INSUFFICIENT_BUFFER from the kernel
        // requires sustained pressure beyond a unit test's reach.
        let err = super::super::error::IocpError::InsufficientBuffer {
            requested: 256,
            capacity: 64,
        };
        let io_err: io::Error = err.into();
        assert_eq!(io_err.kind(), io::ErrorKind::OutOfMemory);
        assert!(io_err.to_string().contains("256"));
    }
}
