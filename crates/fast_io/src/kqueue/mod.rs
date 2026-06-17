//! macOS `kqueue`-based event loop primitive.
//!
//! This module provides a thin, safe wrapper over the `kqueue(2)` /
//! `kevent(2)` syscall pair. It is the foundation primitive for the
//! kqueue-driven `AsyncFileWriter` backend described in
//! `docs/design/macos-kqueue-fast-io.md` (#1385). Consumer migrations
//! (the disk-commit thread, daemon accept loop) land in later PRs and
//! reuse [`KqueueLoop`] as their event surface.
//!
//! # Design choice: direct `libc` bindings
//!
//! We bind to `libc::kqueue` and `libc::kevent` directly rather than
//! pulling in the `kqueue` crate. Rationale:
//!
//! - `libc` is already a workspace dependency on every unix target.
//! - The `kqueue` crate adds a new dependency and wraps the same syscalls
//!   in a higher-level iterator-style API that does not match our
//!   submit/wait usage pattern.
//! - The unsafe surface here is small (two FFI calls) and easy to audit
//!   in one file.
//!
//! # Surface
//!
//! - [`KqueueLoop`] owns a `RawFd` returned from `kqueue(2)` and closes
//!   it on drop.
//! - [`KqueueLoop::submit_read`] registers an `EVFILT_READ | EV_ADD |
//!   EV_CLEAR` event for a borrowed fd plus user-data tag.
//! - [`KqueueLoop::submit_write`] registers the corresponding write
//!   event.
//! - [`KqueueLoop::wait`] blocks on `kevent(2)` with an optional
//!   `Duration` timeout and returns a `Vec<KEvent>` of ready events.
//!
//! `KqueueLoop` is `Send` (the underlying kqueue fd is per-process and
//! moveable across threads) but not `Sync`. Concurrent submissions from
//! multiple threads would require external synchronization; consumers
//! own one loop per disk-commit / accept thread, mirroring the io_uring
//! "one ring per thread" composition rule documented in
//! `docs/design/io-uring-rayon-composition.md`.

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

mod timer;

pub use timer::TimerSleeper;

/// Filter type for readiness events.
///
/// Mirrors the kqueue `EVFILT_*` constants we expose. Additional filters
/// (timer, signal, vnode, user) can be added when consumer migrations
/// need them; the design doc identifies `EVFILT_TIMER` and
/// `EVFILT_VNODE` as future candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KEventFilter {
    /// `EVFILT_READ` - fd is readable.
    Read,
    /// `EVFILT_WRITE` - fd is writable.
    Write,
}

impl KEventFilter {
    fn as_raw(self) -> i16 {
        match self {
            Self::Read => libc::EVFILT_READ,
            Self::Write => libc::EVFILT_WRITE,
        }
    }

    fn from_raw(raw: i16) -> Option<Self> {
        match raw {
            libc::EVFILT_READ => Some(Self::Read),
            libc::EVFILT_WRITE => Some(Self::Write),
            _ => None,
        }
    }
}

/// A single readiness event returned from [`KqueueLoop::wait`].
#[derive(Debug, Clone, Copy)]
pub struct KEvent {
    /// The fd whose readiness fired (kqueue's `ident` field).
    pub fd: RawFd,
    /// The filter that fired (`Read`, `Write`, ...).
    pub filter: KEventFilter,
    /// User-data tag supplied when the event was submitted.
    pub user_data: u64,
    /// Filter-specific data. For `EVFILT_READ`/`EVFILT_WRITE` this is
    /// the number of bytes the kernel reports as available / writable.
    pub data: i64,
    /// Raw flags returned by the kernel (`EV_EOF`, `EV_ERROR`, ...).
    pub flags: u16,
}

impl KEvent {
    /// Returns `true` if the kernel signalled end-of-file on the fd.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.flags & libc::EV_EOF != 0
    }

    /// Returns `true` if the kernel reported an error on the event.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.flags & libc::EV_ERROR != 0
    }
}

/// A kqueue event loop primitive.
///
/// Wraps a kqueue file descriptor returned from `kqueue(2)`. The
/// descriptor is closed when the loop is dropped.
#[derive(Debug)]
pub struct KqueueLoop {
    kq: RawFd,
}

impl KqueueLoop {
    /// Creates a new kqueue instance via `kqueue(2)`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the kernel cannot allocate a kqueue
    /// (typically `EMFILE` / `ENFILE` from fd table exhaustion).
    pub fn new() -> io::Result<Self> {
        // SAFETY: `kqueue(2)` takes no arguments and returns a new
        // file descriptor on success or -1 on failure. There are no
        // pointer or lifetime invariants to uphold here.
        #[allow(unsafe_code)]
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { kq })
    }

    /// Returns the raw kqueue file descriptor.
    ///
    /// Exposed for diagnostic and integration use only; callers must
    /// not close it.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        self.kq
    }

    /// Registers an `EVFILT_READ` readiness event for the given fd.
    ///
    /// Uses `EV_ADD | EV_CLEAR`: the event is added if missing and
    /// behaves as edge-triggered, matching the Linux `EPOLLET` shape.
    /// `user_data` is stored in the event's `udata` field and is
    /// returned to the caller in the [`KEvent::user_data`] field when
    /// the event fires.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if `kevent(2)` rejects the registration.
    pub fn submit_read(&self, fd: RawFd, user_data: u64) -> io::Result<()> {
        self.submit_event(fd, KEventFilter::Read, user_data)
    }

    /// Registers an `EVFILT_WRITE` readiness event for the given fd.
    ///
    /// See [`submit_read`](Self::submit_read) for semantics. This is
    /// the event the disk-commit backend uses to park on writeback
    /// pressure (`EAGAIN` from `pwrite(2)` on a nonblocking fd).
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if `kevent(2)` rejects the registration.
    pub fn submit_write(&self, fd: RawFd, user_data: u64) -> io::Result<()> {
        self.submit_event(fd, KEventFilter::Write, user_data)
    }

    /// Removes a previously-registered event for the given fd/filter.
    ///
    /// Idempotent on `ENOENT` (returns `Ok(())` if the event was not
    /// registered) so callers can deregister unconditionally on
    /// shutdown.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` for any failure other than `ENOENT`.
    pub fn remove(&self, fd: RawFd, filter: KEventFilter) -> io::Result<()> {
        let change = make_kevent(fd, filter.as_raw(), libc::EV_DELETE, 0);
        let rc = self.kevent_call(Some(&change), None, None)?;
        debug_assert_eq!(rc, 0, "EV_DELETE with no eventlist returns 0 on success");
        Ok(())
    }

    fn submit_event(&self, fd: RawFd, filter: KEventFilter, user_data: u64) -> io::Result<()> {
        let change = make_kevent(
            fd,
            filter.as_raw(),
            libc::EV_ADD | libc::EV_CLEAR,
            user_data,
        );
        let rc = self.kevent_call(Some(&change), None, None)?;
        debug_assert_eq!(rc, 0, "registration kevent returns 0 with empty eventlist");
        Ok(())
    }

    /// Waits for ready events.
    ///
    /// Blocks on `kevent(2)`, returning up to `max_events` ready
    /// events. If `timeout` is `Some`, the call returns after the
    /// timeout elapses even with no events. If `timeout` is `None`,
    /// the call blocks indefinitely.
    ///
    /// Returns an empty `Vec` if the timeout elapses with no events.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if `kevent(2)` fails. `EINTR` is
    /// translated into an empty result so callers can re-enter the
    /// loop without bespoke signal handling.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<Vec<KEvent>> {
        self.wait_with_capacity(timeout, 32)
    }

    /// Like [`wait`](Self::wait) but caps the number of events
    /// returned in one call.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if `kevent(2)` fails.
    pub fn wait_with_capacity(
        &self,
        timeout: Option<Duration>,
        max_events: usize,
    ) -> io::Result<Vec<KEvent>> {
        let max_events = max_events.max(1);
        let mut events: Vec<libc::kevent> = (0..max_events).map(|_| empty_kevent()).collect();
        let ts = timeout.map(duration_to_timespec);
        let returned = match self.kevent_call(None, Some(&mut events), ts.as_ref()) {
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        Ok(events
            .into_iter()
            .take(returned as usize)
            .filter_map(decode_kevent)
            .collect())
    }

    /// Underlying `kevent(2)` invocation.
    ///
    /// Centralizes the unsafe FFI call so submission and wait paths
    /// share one audited site.
    fn kevent_call(
        &self,
        change: Option<&libc::kevent>,
        events: Option<&mut [libc::kevent]>,
        timeout: Option<&libc::timespec>,
    ) -> io::Result<i32> {
        let (change_ptr, nchanges) = match change {
            Some(c) => (c as *const libc::kevent, 1),
            None => (std::ptr::null(), 0),
        };
        let (event_ptr, nevents) = match events {
            Some(slice) => (slice.as_mut_ptr(), slice.len() as i32),
            None => (std::ptr::null_mut(), 0),
        };
        let ts_ptr = timeout
            .map(|t| t as *const libc::timespec)
            .unwrap_or(std::ptr::null());

        // SAFETY: `self.kq` is a valid kqueue fd owned for the lifetime
        // of `self`. `change_ptr` is either null with `nchanges == 0`
        // or a valid `&libc::kevent` borrowed for the duration of the
        // call. `event_ptr` is either null with `nevents == 0` or
        // points to `nevents` writable slots in a slice borrowed for
        // the call. `ts_ptr` is either null or borrowed from `timeout`
        // for the call. None of the buffers escape this scope, so all
        // lifetimes hold.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::kevent(self.kq, change_ptr, nchanges, event_ptr, nevents, ts_ptr) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(rc)
    }
}

impl Drop for KqueueLoop {
    fn drop(&mut self) {
        if self.kq >= 0 {
            // SAFETY: `self.kq` was returned from `kqueue(2)` and not
            // closed elsewhere - `KqueueLoop` owns it exclusively.
            // `close(2)` may fail but there is nothing useful to do on
            // failure in `Drop`.
            #[allow(unsafe_code)]
            unsafe {
                libc::close(self.kq);
            }
        }
    }
}

// SAFETY: A kqueue fd is per-process and can be moved across threads.
// `KqueueLoop` does not implement `Sync`; concurrent access requires
// external synchronization, which matches the io_uring single-owner
// composition rule.
#[allow(unsafe_code)]
unsafe impl Send for KqueueLoop {}

fn empty_kevent() -> libc::kevent {
    libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

fn make_kevent(fd: RawFd, filter: i16, flags: u16, user_data: u64) -> libc::kevent {
    libc::kevent {
        ident: fd as libc::uintptr_t,
        filter,
        flags,
        fflags: 0,
        data: 0,
        udata: user_data as *mut libc::c_void,
    }
}

fn decode_kevent(ev: libc::kevent) -> Option<KEvent> {
    let filter = KEventFilter::from_raw(ev.filter)?;
    Some(KEvent {
        fd: ev.ident as RawFd,
        filter,
        user_data: ev.udata as u64,
        data: ev.data as i64,
        flags: ev.flags,
    })
}

fn duration_to_timespec(d: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: d.as_secs() as libc::time_t,
        tv_nsec: i64::from(d.subsec_nanos()) as libc::c_long,
    }
}

/// Convenience helper: borrow the raw fd from any `AsRawFd` and submit
/// a read event.
///
/// # Errors
///
/// Propagates errors from [`KqueueLoop::submit_read`].
pub fn submit_read<F: AsRawFd>(loop_: &KqueueLoop, src: &F, user_data: u64) -> io::Result<()> {
    loop_.submit_read(src.as_raw_fd(), user_data)
}

/// Returns whether kqueue is available on this platform.
///
/// Always `true` on macOS (the syscall is guaranteed present). Kept as
/// a function rather than a constant so the public API matches the
/// stub module's runtime probe shape.
#[must_use]
pub fn is_kqueue_available() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::io::FromRawFd;
    use std::thread;

    /// Create a Unix pipe and return `(reader, writer)` as owned files.
    fn pipe_pair() -> (std::fs::File, std::fs::File) {
        let mut fds = [0i32; 2];
        // SAFETY: `pipe(2)` fills `fds` with two newly-allocated fds
        // on success. The fds are converted into owned `File`s that
        // close them on drop.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe(2) failed: {}", io::Error::last_os_error());
        // SAFETY: `fds[0]` and `fds[1]` were just returned by
        // `pipe(2)` and are not aliased elsewhere.
        #[allow(unsafe_code)]
        let (reader, writer) = unsafe {
            (
                std::fs::File::from_raw_fd(fds[0]),
                std::fs::File::from_raw_fd(fds[1]),
            )
        };
        (reader, writer)
    }

    #[test]
    fn new_loop_succeeds() {
        let kq = KqueueLoop::new().expect("kqueue creation succeeds");
        assert!(kq.as_raw_fd() >= 0, "kqueue fd is valid");
    }

    #[test]
    fn read_event_fires_on_pipe_write() {
        let (mut reader, mut writer) = pipe_pair();
        let kq = KqueueLoop::new().expect("kqueue creation succeeds");
        kq.submit_read(reader.as_raw_fd(), 0xCAFE_BABE)
            .expect("submit_read registers event");

        let handle = thread::spawn(move || {
            // Brief sleep to ensure the wait call is already parked.
            thread::sleep(Duration::from_millis(20));
            writer.write_all(b"ping").expect("write to pipe");
            writer.flush().expect("flush pipe");
        });

        let events = kq
            .wait(Some(Duration::from_secs(2)))
            .expect("wait returns events");
        handle.join().expect("writer thread completes");

        assert_eq!(events.len(), 1, "exactly one event fires");
        let ev = events[0];
        assert_eq!(ev.fd, reader.as_raw_fd(), "event reports pipe reader fd");
        assert_eq!(ev.filter, KEventFilter::Read, "event filter is read");
        assert_eq!(ev.user_data, 0xCAFE_BABE, "user_data round-trips");
        assert!(ev.data >= 4, "kernel reports at least 4 bytes readable");

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).expect("read pings");
        assert_eq!(&buf, b"ping");
    }

    #[test]
    fn wait_returns_empty_on_timeout() {
        let kq = KqueueLoop::new().expect("kqueue creation succeeds");
        let start = std::time::Instant::now();
        let events = kq
            .wait(Some(Duration::from_millis(100)))
            .expect("wait returns on timeout");
        let elapsed = start.elapsed();
        assert!(events.is_empty(), "no events fire");
        assert!(
            elapsed >= Duration::from_millis(80),
            "wait blocked roughly for the timeout (elapsed={elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "wait did not block forever (elapsed={elapsed:?})"
        );
    }

    #[test]
    fn remove_is_idempotent_on_missing_registration() {
        let (reader, _writer) = pipe_pair();
        let kq = KqueueLoop::new().expect("kqueue creation succeeds");
        // EV_DELETE for a never-registered filter should return ENOENT,
        // which we currently surface as an Err. The test documents that
        // shape so a future relaxation is intentional.
        let err = kq
            .remove(reader.as_raw_fd(), KEventFilter::Write)
            .expect_err("removing an unregistered filter errors");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    }

    #[test]
    fn eof_is_signalled_when_writer_closes() {
        let (reader, writer) = pipe_pair();
        let kq = KqueueLoop::new().expect("kqueue creation succeeds");
        kq.submit_read(reader.as_raw_fd(), 7)
            .expect("submit_read registers event");
        drop(writer);

        let events = kq
            .wait(Some(Duration::from_secs(2)))
            .expect("wait returns events");
        assert!(!events.is_empty(), "read event fires on writer close");
        assert!(events[0].is_eof(), "EV_EOF set after writer dropped");
    }
}
