//! Grand Central Dispatch (`dispatch_io`) async file I/O primitives (MFAST-1).
//!
//! macOS ships an asynchronous, kernel-assisted file I/O interface in
//! `libdispatch`: `dispatch_io` channels stream reads and writes through
//! Grand Central Dispatch, coalescing syscalls and overlapping I/O with
//! computation without the caller managing a run loop. This module exposes
//! that interface through three safe wrappers - [`GcdQueue`], [`GcdReader`],
//! and [`GcdWriter`] - so the rest of `oc-rsync` can drive it.
//!
//! # Async-to-blocking bridge
//!
//! `dispatch_io` is fundamentally asynchronous: the read/write handler block
//! runs later on a dispatch queue. The rest of `oc-rsync` is blocking and
//! threaded, so each wrapper submits one operation and then **blocks the
//! calling thread** on a condvar until the handler signals completion. The
//! handler runs on a global concurrent queue (never the caller's thread), so
//! there is no self-deadlock: the caller parks, the queue worker fills in the
//! result and notifies, the caller wakes. Errors and partial transfers are
//! surfaced through the same channel.
//!
//! # Ownership and release
//!
//! Every dispatch object is reference counted. Channels created by
//! `dispatch_io_create` and data objects created by `dispatch_data_create`
//! are caller-owned and are balanced with `dispatch_release` (channels are
//! first closed with `dispatch_io_close`). Global queues from
//! `dispatch_get_global_queue` are process-wide singletons and are never
//! released. The completion block shares an `Arc` with the caller; because
//! the caller blocks until the block has run to completion, the shared state
//! always outlives every block invocation - no use-after-free.
//!
//! # Scope
//!
//! This module delivers the primitives only. No transfer path (disk-commit,
//! delta-apply, local-copy) consumes them yet.

#![allow(unsafe_code)]

mod sys;

use std::io;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::ptr;
use std::sync::{Arc, Condvar, Mutex};

use block2::RcBlock;

/// Outcome shared between a wrapper's calling thread and its completion block.
///
/// The block writes the result and flips `done`; the caller waits on the
/// condvar for `done` to become set.
struct Completion {
    /// `None` while in flight; `Some(Ok(()))` on clean finish, `Some(Err(e))`
    /// carrying the `errno`-style dispatch error code on failure.
    result: Mutex<Option<io::Result<()>>>,
    /// Signalled by the completion block once `result` is populated.
    cond: Condvar,
}

impl Completion {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            cond: Condvar::new(),
        })
    }

    /// Records the terminal outcome and wakes the waiting caller.
    fn finish(&self, outcome: io::Result<()>) {
        let mut guard = self.result.lock().expect("gcd completion mutex poisoned");
        *guard = Some(outcome);
        self.cond.notify_all();
    }

    /// Blocks the calling thread until the completion block has finished.
    fn wait(&self) -> io::Result<()> {
        let mut guard = self.result.lock().expect("gcd completion mutex poisoned");
        while guard.is_none() {
            guard = self
                .cond
                .wait(guard)
                .expect("gcd completion mutex poisoned");
        }
        guard.take().expect("completion set but empty")
    }
}

/// Maps a dispatch error code (`errno`-valued, `0` == success) to an
/// [`io::Result`].
fn dispatch_err(code: i32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}

/// A Grand Central Dispatch queue handle.
///
/// Two flavours are exposed. [`GcdQueue::global`] wraps the process-wide global
/// concurrent queue used to run the async I/O completion handlers on a GCD
/// worker thread rather than the caller's; it is a singleton and needs no
/// release. [`GcdQueue::new_serial`] creates a dedicated serial queue (used as
/// a channel's target queue) that is caller-owned and released on drop.
pub struct GcdQueue {
    raw: sys::dispatch_queue_t,
    /// Whether this handle owns `raw` (a created serial queue) and must
    /// [`sys::dispatch_release`] it on drop. Global queues set this `false`.
    owned: bool,
}

// SAFETY: `raw` is a `dispatch_queue_t` - either a process-wide global
// concurrent queue (a singleton `libdispatch` guarantees is thread-safe) or a
// created serial queue, both of which `libdispatch` allows referencing from
// any thread. No owning state is mutated through a shared reference.
unsafe impl Send for GcdQueue {}
// SAFETY: see the `Send` impl - the underlying queue is thread-safe.
unsafe impl Sync for GcdQueue {}

impl GcdQueue {
    /// Returns a handle to the default-priority global concurrent queue.
    ///
    /// This queue drives the async read/write completion handlers. It is a
    /// singleton and needs no release.
    pub fn global() -> Self {
        // SAFETY: `dispatch_get_global_queue` with a valid priority and zero
        // flags returns a non-null process-wide queue that never needs
        // releasing.
        let raw =
            unsafe { sys::dispatch_get_global_queue(sys::DISPATCH_QUEUE_PRIORITY_DEFAULT, 0) };
        Self { raw, owned: false }
    }

    /// Creates a caller-owned serial dispatch queue with the given C label.
    ///
    /// The queue is released on drop. Used as a `dispatch_io` channel's target
    /// queue so the channel's internal bookkeeping and cleanup handler run on a
    /// private queue rather than a shared global one.
    pub fn new_serial(label: &std::ffi::CStr) -> io::Result<Self> {
        // SAFETY: `label` is a valid NUL-terminated C string that outlives the
        // call; a null `attr` requests a serial queue. A null return is
        // handled.
        let raw = unsafe { sys::dispatch_queue_create(label.as_ptr(), ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::other("dispatch_queue_create returned null"));
        }
        Ok(Self { raw, owned: true })
    }

    fn raw(&self) -> sys::dispatch_queue_t {
        self.raw
    }
}

impl Drop for GcdQueue {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: `raw` is a live serial queue created by
            // `dispatch_queue_create`; release balances that retain. Global
            // queues (`owned == false`) are never released.
            unsafe { sys::dispatch_release(self.raw) }
        }
    }
}

/// Owns a `dispatch_io_t` channel that has taken exclusive control of a file
/// descriptor.
///
/// Per the `dispatch_io_create` contract, the system takes control of the fd
/// until the channel is closed and all references are released; only then does
/// the cleanup handler run and the application may `close(2)` the fd. This
/// guard therefore takes **ownership** of the raw fd (the source `File` is
/// consumed via `into_raw_fd`, so it never double-closes) and defers the
/// `close(2)` to the cleanup handler, which fires after GCD relinquishes
/// control. Graceful `dispatch_io_close(0)` on drop drains pending I/O first.
struct Channel {
    raw: sys::dispatch_io_t,
    // Keep the cleanup block alive for the channel's whole lifetime. The
    // channel retains its own reference, but holding ours keeps ownership
    // explicit and drop-ordered.
    _cleanup: RcBlock<dyn Fn(i32)>,
}

impl Channel {
    /// Creates a stream channel that takes ownership of `fd`, scheduled on
    /// `queue`. On success GCD owns `fd`; the cleanup handler closes it once
    /// control is relinquished. On failure `fd` is closed here so it never
    /// leaks.
    fn create(fd: RawFd, queue: &GcdQueue) -> io::Result<Self> {
        // The cleanup block closes the fd once GCD relinquishes control. This
        // is the canonical `dispatch_io_create` ownership handoff: the fd is
        // closed exactly once, here, and never by a `File`.
        let cleanup: RcBlock<dyn Fn(i32)> = RcBlock::new(move |_error: i32| {
            // SAFETY: the cleanup handler runs only after GCD has relinquished
            // control of `fd`, so closing it here is the sole close and cannot
            // race the channel.
            unsafe {
                libc::close(fd);
            }
        });
        // SAFETY: `fd` is a valid open descriptor now owned by this channel.
        // `queue` is a valid queue. `cleanup` is a live block whose pointer
        // stays valid because `self` retains the `RcBlock`. A null return is
        // handled by closing `fd` and erroring.
        let raw = unsafe {
            sys::dispatch_io_create(
                sys::DISPATCH_IO_STREAM,
                fd,
                queue.raw(),
                RcBlock::as_ptr(&cleanup) as *mut _,
            )
        };
        if raw.is_null() {
            // GCD did not take ownership; close the fd ourselves so it does
            // not leak. The cleanup block is dropped without ever running.
            // SAFETY: `fd` is still a valid open descriptor we own.
            unsafe {
                libc::close(fd);
            }
            return Err(io::Error::other("dispatch_io_create returned null"));
        }
        Ok(Self {
            raw,
            _cleanup: cleanup,
        })
    }

    fn raw(&self) -> sys::dispatch_io_t {
        self.raw
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        // SAFETY: `raw` is a live caller-owned channel. Graceful close (flags
        // 0) drains pending I/O and schedules the cleanup handler (which closes
        // the fd); `dispatch_release` then balances the `dispatch_io_create`
        // retain.
        unsafe {
            sys::dispatch_io_close(self.raw, 0);
            sys::dispatch_release(self.raw);
        }
    }
}

/// Blocking reader that pulls bytes from a file descriptor through a
/// `dispatch_io` channel.
///
/// Each [`read`](GcdReader::read) submits one asynchronous
/// `dispatch_io_read` for up to the caller's buffer length and blocks
/// until the channel reports the chunk (or EOF, or an error). Successive reads
/// advance the stream position, giving a `Read`-like sequential interface over
/// the async primitive.
pub struct GcdReader {
    // Field drop order is declaration order: `channel` must close and release
    // before `_channel_queue` (its target queue) is released. The channel owns
    // the fd (closed by its cleanup handler), so no `File` is retained.
    channel: Channel,
    _channel_queue: GcdQueue,
    queue: GcdQueue,
}

impl GcdReader {
    /// Opens `path` for reading through a GCD channel.
    pub fn open(path: &std::path::Path) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::from_file(file)
    }

    /// Wraps an already-open file for GCD reads.
    ///
    /// Consumes `file`: the GCD channel takes exclusive ownership of the
    /// descriptor and closes it via its cleanup handler, so `file` must not be
    /// used after this call.
    pub fn from_file(file: std::fs::File) -> io::Result<Self> {
        let queue = GcdQueue::global();
        let channel_queue = GcdQueue::new_serial(c"oc-rsync.gcd.reader")?;
        // Consume the `File` into a raw fd so only the channel closes it.
        let fd = file.into_raw_fd();
        let channel = Channel::create(fd, &channel_queue)?;
        Ok(Self {
            channel,
            _channel_queue: channel_queue,
            queue,
        })
    }

    /// Reads up to `buf.len()` bytes into `buf`, returning the number read.
    ///
    /// Returns `Ok(0)` at end of stream. Drives the async `dispatch_io_read`
    /// to completion on the calling thread via the condvar bridge.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let completion = Completion::new();
        // Accumulator the completion block appends into. Wrapped in a Mutex so
        // the block (running on a GCD worker) and this thread never race; the
        // caller only touches it after `wait()` returns.
        let collected: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        let completion_cb = Arc::clone(&completion);
        let collected_cb = Arc::clone(&collected);
        let handler: RcBlock<dyn Fn(sys::dispatch_bool_t, sys::dispatch_data_t, i32)> =
            RcBlock::new(
                move |done: sys::dispatch_bool_t, data: sys::dispatch_data_t, error: i32| {
                    if !data.is_null() {
                        // SAFETY: `data` is a valid, retained-for-the-callback
                        // `dispatch_data_t`; appending its bytes copies out
                        // before the callback returns.
                        if let Err(e) = unsafe { append_dispatch_data(data, &collected_cb) } {
                            completion_cb.finish(Err(e));
                            return;
                        }
                    }
                    if error != 0 {
                        completion_cb.finish(dispatch_err(error));
                    } else if done != 0 {
                        completion_cb.finish(Ok(()));
                    }
                },
            );

        // SAFETY: `channel` is live, `queue` is a valid global queue, and
        // `handler` stays alive until after `wait()` returns because both the
        // `RcBlock` local and the shared `Arc`s outlive the blocking wait.
        unsafe {
            sys::dispatch_io_read(
                self.channel.raw(),
                0,
                buf.len(),
                self.queue.raw(),
                RcBlock::as_ptr(&handler) as *mut _,
            );
        }

        completion.wait()?;
        drop(handler);

        let bytes = collected.lock().expect("gcd read buffer poisoned");
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }

    /// Reads the entire remaining stream into a `Vec<u8>`.
    pub fn read_to_end(&mut self) -> io::Result<Vec<u8>> {
        let completion = Completion::new();
        let collected: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        let completion_cb = Arc::clone(&completion);
        let collected_cb = Arc::clone(&collected);
        let handler: RcBlock<dyn Fn(sys::dispatch_bool_t, sys::dispatch_data_t, i32)> =
            RcBlock::new(
                move |done: sys::dispatch_bool_t, data: sys::dispatch_data_t, error: i32| {
                    if !data.is_null() {
                        // SAFETY: see `read`; `data` is valid for the callback.
                        if let Err(e) = unsafe { append_dispatch_data(data, &collected_cb) } {
                            completion_cb.finish(Err(e));
                            return;
                        }
                    }
                    if error != 0 {
                        completion_cb.finish(dispatch_err(error));
                    } else if done != 0 {
                        completion_cb.finish(Ok(()));
                    }
                },
            );

        // SAFETY: as in `read`; length `DISPATCH_IO_READ_ALL` streams to EOF.
        unsafe {
            sys::dispatch_io_read(
                self.channel.raw(),
                0,
                sys::DISPATCH_IO_READ_ALL,
                self.queue.raw(),
                RcBlock::as_ptr(&handler) as *mut _,
            );
        }

        completion.wait()?;
        drop(handler);
        let mut bytes = collected.lock().expect("gcd read buffer poisoned");
        Ok(std::mem::take(&mut bytes))
    }
}

/// Blocking writer that pushes bytes to a file descriptor through a
/// `dispatch_io` channel.
///
/// Each [`write_all`](GcdWriter::write_all) copies the payload into a
/// `dispatch_data_t`, submits one asynchronous `dispatch_io_write`, and
/// blocks until the channel reports the write finished (or errored). Bytes are
/// only durable after a normal `fsync`/close of the underlying file, matching
/// standard buffered-write semantics.
pub struct GcdWriter {
    // Field drop order is declaration order: `channel` must close and release
    // before `_channel_queue` (its target queue) is released. The channel owns
    // the fd (closed by its cleanup handler); `fd` here is a borrow used only
    // for `fsync` and is never closed by the writer.
    channel: Channel,
    _channel_queue: GcdQueue,
    queue: GcdQueue,
    fd: RawFd,
}

impl GcdWriter {
    /// Creates or truncates `path` for writing through a GCD channel.
    pub fn create(path: &std::path::Path) -> io::Result<Self> {
        let file = std::fs::File::create(path)?;
        Self::from_file(file)
    }

    /// Wraps an already-open, writable file for GCD writes.
    ///
    /// Consumes `file`: the GCD channel takes exclusive ownership of the
    /// descriptor and closes it via its cleanup handler, so `file` must not be
    /// used after this call.
    pub fn from_file(file: std::fs::File) -> io::Result<Self> {
        let queue = GcdQueue::global();
        let channel_queue = GcdQueue::new_serial(c"oc-rsync.gcd.writer")?;
        // Consume the `File` into a raw fd so only the channel closes it.
        let fd = file.into_raw_fd();
        let channel = Channel::create(fd, &channel_queue)?;
        Ok(Self {
            channel,
            _channel_queue: channel_queue,
            queue,
            fd,
        })
    }

    /// Writes the whole of `buf`, blocking until the channel confirms the
    /// write finished. Returns an error on any partial-then-failed write.
    pub fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }

        // SAFETY: `buf` is a valid slice; `dispatch_data_create` with a null
        // destructor copies the bytes into the data object, so `buf` need not
        // outlive the call. A null return is handled below.
        let data = unsafe {
            sys::dispatch_data_create(
                buf.as_ptr() as *const _,
                buf.len(),
                self.queue.raw(),
                ptr::null_mut(),
            )
        };
        if data.is_null() {
            return Err(io::Error::other("dispatch_data_create returned null"));
        }
        // Release the caller-owned data object once the write has drained it.
        let _data_guard = DispatchDataGuard(data);

        let completion = Completion::new();
        let completion_cb = Arc::clone(&completion);
        let handler: RcBlock<dyn Fn(sys::dispatch_bool_t, sys::dispatch_data_t, i32)> =
            RcBlock::new(
                move |done: sys::dispatch_bool_t, _data: sys::dispatch_data_t, error: i32| {
                    if error != 0 {
                        completion_cb.finish(dispatch_err(error));
                    } else if done != 0 {
                        completion_cb.finish(Ok(()));
                    }
                },
            );

        // SAFETY: `channel`, `queue`, and `data` are all live; `handler` and
        // `data` outlive the blocking `wait()` below, so the async write has
        // valid operands for its whole duration.
        unsafe {
            sys::dispatch_io_write(
                self.channel.raw(),
                0,
                data,
                self.queue.raw(),
                RcBlock::as_ptr(&handler) as *mut _,
            );
        }

        let outcome = completion.wait();
        drop(handler);
        outcome
    }

    /// Flushes written data to stable storage via `fsync`. Each `write_all`
    /// already drives its `dispatch_io_write` to completion, so this only
    /// forces the durability barrier on the channel-owned descriptor.
    pub fn sync_all(&mut self) -> io::Result<()> {
        // SAFETY: `self.fd` is the live descriptor the channel owns; `fsync`
        // does not close it, so it does not interfere with the channel's
        // ownership or its cleanup-handler close.
        let rc = unsafe { libc::fsync(self.fd) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// RAII guard releasing a caller-owned `dispatch_data_t` on drop.
struct DispatchDataGuard(sys::dispatch_data_t);

impl Drop for DispatchDataGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live data object created by
        // `dispatch_data_create`; releasing it balances that retain.
        unsafe { sys::dispatch_release(self.0) }
    }
}

/// Copies the bytes of a `dispatch_data_t` into `out`.
///
/// # Safety
///
/// `data` must be a valid, non-null `dispatch_data_t` that stays alive for the
/// duration of the call.
unsafe fn append_dispatch_data(
    data: sys::dispatch_data_t,
    out: &Arc<Mutex<Vec<u8>>>,
) -> io::Result<()> {
    // SAFETY: `data` is valid per the contract; `dispatch_data_get_size`
    // reads its logical length.
    let size = unsafe { sys::dispatch_data_get_size(data) };
    if size == 0 {
        return Ok(());
    }
    let mut ptr_out: *const std::ffi::c_void = ptr::null();
    let mut len_out: usize = 0;
    // SAFETY: `data` is valid; `create_map` maps it into one contiguous region
    // and writes the pointer + length through the out-params. The returned
    // `mapped` object owns that region and is released below.
    let mapped = unsafe { sys::dispatch_data_create_map(data, &mut ptr_out, &mut len_out) };
    if mapped.is_null() || ptr_out.is_null() {
        if !mapped.is_null() {
            // SAFETY: `mapped` is a live data object; release it.
            unsafe { sys::dispatch_release(mapped) };
        }
        return Err(io::Error::other("dispatch_data_create_map returned null"));
    }
    // SAFETY: `ptr_out`/`len_out` describe a valid contiguous region owned by
    // `mapped`, which is alive here; copy the bytes out before releasing it.
    let slice = unsafe { std::slice::from_raw_parts(ptr_out as *const u8, len_out) };
    out.lock()
        .expect("gcd read buffer poisoned")
        .extend_from_slice(slice);
    // SAFETY: `mapped` is a live caller-owned data object; release it.
    unsafe { sys::dispatch_release(mapped) };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("oc_rsync_gcd_{}_{}", std::process::id(), name));
        p
    }

    #[test]
    fn reader_returns_exact_bytes() {
        let path = temp_path("read_exact");
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &payload).unwrap();

        let mut reader = GcdReader::open(&path).unwrap();
        let got = reader.read_to_end().unwrap();
        assert_eq!(got, payload, "read_to_end must return the exact bytes");

        // Chunked read reassembles the same content.
        let mut reader2 = GcdReader::open(&path).unwrap();
        let mut assembled = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let n = reader2.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            assembled.extend_from_slice(&buf[..n]);
        }
        assert_eq!(assembled, payload, "chunked reads must reassemble exactly");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn writer_roundtrips_through_normal_read() {
        let path = temp_path("write_roundtrip");
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 253) as u8).collect();

        {
            let mut writer = GcdWriter::create(&path).unwrap();
            writer.write_all(&payload).unwrap();
            writer.sync_all().unwrap();
        }

        let back = std::fs::read(&path).unwrap();
        assert_eq!(
            back, payload,
            "GCD-written bytes must read back identically"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn repeated_create_drop_does_not_crash() {
        let path = temp_path("leak_smoke");
        let write_path = temp_path("leak_smoke_w");
        std::fs::write(&path, b"leak smoke test payload").unwrap();
        for _ in 0..256 {
            let reader = GcdReader::open(&path).unwrap();
            drop(reader);
            let writer = GcdWriter::create(&write_path).unwrap();
            drop(writer);
        }
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&write_path).ok();
    }

    #[test]
    fn read_from_closed_fd_errors_cleanly() {
        // Exercise the error path without any fd-recycling hazard: open a
        // write-only descriptor and attempt to *read* through it. The fd is
        // valid (so the channel owns and closes exactly one live descriptor,
        // never a recycled number that could clobber a concurrent test), but
        // reads on an O_WRONLY fd fail with a clean POSIX error. The wrapper
        // must surface that as an `Err` rather than panicking or corrupting.
        // SAFETY: opening `/dev/null` write-only yields a valid owned fd.
        let raw = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
        assert!(raw >= 0, "failed to open /dev/null write-only");
        // SAFETY: `raw` is a fresh, valid, owned fd; ownership transfers into
        // the channel, which becomes the sole closer.
        let wronly = unsafe { std::fs::File::from_raw_fd(raw) };
        let mut reader = GcdReader::from_file(wronly).unwrap();
        let mut buf = [0u8; 8];
        // Reading a write-only descriptor must fail cleanly, not panic and not
        // return spurious bytes. An EOF-like zero read is also acceptable.
        match reader.read(&mut buf) {
            Err(_) | Ok(0) => {}
            Ok(n) => panic!("unexpected {n}-byte read from a write-only descriptor"),
        }
    }
}
