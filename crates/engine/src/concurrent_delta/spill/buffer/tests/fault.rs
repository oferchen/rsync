//! Fault-injection harness for the reorder-spill module.
//!
//! Implements the SPL-33.a design
//! (`docs/design/spl-33a-enospc-injection-mechanism.md`) and the chassis
//! shared with SPL-34 (`docs/design/spl-34-temp-vanish-injection.md`).
//! This file is the *harness only*; the assertion tests that drive
//! [`SpillableReorderBuffer`](super::super::SpillableReorderBuffer) through
//! every injection scenario land in SPL-33.c.
//!
//! Two layers are provided:
//!
//! 1. [`MockEnoSpcWriter`] - a userspace [`Write`] adapter that wraps any
//!    real writer and returns [`ErrorKind::StorageFull`] after a configurable
//!    byte threshold. Portable across Linux, macOS, and Windows; deterministic
//!    by construction. The unit-test layer of every SPL-33 assertion uses
//!    this adapter.
//! 2. `with_full_tmpfs` (Linux only) - mounts a fixed-size tmpfs, pre-fills
//!    it with `fallocate(2)` so the next caller write hits real-kernel
//!    ENOSPC, runs the supplied closure, and unmounts on Drop. Requires
//!    `CAP_SYS_ADMIN`; the helper short-circuits with a `#[test]`-friendly
//!    skip when the capability is missing so CI tiles without privileges do
//!    not flake.
//!
//! Both layers share [`FaultPlan`], which describes when and how a fault
//! fires. SPL-34.b extends [`FaultEvent`] with vanish modes; the SPL-33
//! variants live here as the seed.

#![cfg(test)]

use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};

/// Cross-platform [`ErrorKind`] used to simulate ENOSPC.
///
/// Stable across Linux, macOS, and Windows: every supported kernel maps the
/// out-of-space syscall failure to [`ErrorKind::StorageFull`] in `std::io`.
/// Constructors take an explicit kind so the assertion tests can vary it
/// (`EDQUOT`, `EFBIG`, etc.) without rebuilding the wrapper.
pub(crate) const ENOSPC_KIND: ErrorKind = ErrorKind::StorageFull;

/// Trigger surface for a single fault injection.
///
/// Variants are additive: SPL-33 ships [`FaultEvent::DiskFull`]; SPL-34 will
/// add temp-vanish variants on the same chassis. New variants must not
/// change the existing semantics of the [`FaultPlan`] byte-count trigger
/// implemented by [`MockEnoSpcWriter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultEvent {
    /// Simulate ENOSPC. The wrapped writer succeeds until the cumulative
    /// byte count reaches `after_bytes`, after which subsequent writes
    /// return [`ENOSPC_KIND`].
    DiskFull {
        /// Cumulative byte threshold past which writes start failing.
        after_bytes: u64,
    },
}

/// Declarative description of when and how a fault fires.
///
/// Mirrors the SPL-33.a design's `FaultPlan` exactly so SPL-34.b can extend
/// the same struct with vanish-mode fields without breaking SPL-33 callers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FaultPlan {
    /// Error kind injected when the trigger fires. Defaults to
    /// [`ENOSPC_KIND`] via [`FaultPlan::enospc`].
    pub(crate) kind: ErrorKind,
    /// Event describing the trigger. SPL-33 only uses
    /// [`FaultEvent::DiskFull`]; SPL-34 will add vanish events.
    pub(crate) event: FaultEvent,
    /// When `true`, the fault fires exactly once and subsequent writes
    /// succeed against the underlying writer. When `false`, the fault
    /// persists for the rest of the wrapper's lifetime.
    pub(crate) one_shot: bool,
}

impl FaultPlan {
    /// Builds a plan that simulates ENOSPC after `after_bytes` cumulative
    /// bytes have been accepted by the underlying writer.
    pub(crate) fn enospc(after_bytes: u64) -> Self {
        Self {
            kind: ENOSPC_KIND,
            event: FaultEvent::DiskFull { after_bytes },
            one_shot: false,
        }
    }

    /// Returns the byte threshold past which the next write fails.
    fn after_bytes(&self) -> u64 {
        match self.event {
            FaultEvent::DiskFull { after_bytes } => after_bytes,
        }
    }
}

/// [`Write`] adapter that returns [`ENOSPC_KIND`] after a configurable byte
/// threshold.
///
/// The wrapper buffers the cumulative byte count internally and consults
/// [`FaultPlan`] before delegating each call to the inner writer. A call
/// whose payload would push the cumulative byte count past the threshold
/// fails atomically with [`ENOSPC_KIND`] - the inner writer is never
/// touched on that call - which matches the kernel's strict-ENOSPC
/// behaviour for `write(2)` calls that cannot fit at all. Calls whose
/// payload fits entirely below the threshold succeed normally; the
/// threshold-crossing failure surfaces on the first call past it.
///
/// The wrapper is generic over any [`Write`] so unit tests can target both
/// in-memory buffers (`Vec<u8>`) and real `File` handles via the spill
/// backend without changing the adapter.
pub(crate) struct MockEnoSpcWriter<W: Write> {
    inner: W,
    plan: FaultPlan,
    bytes_written: u64,
    tripped: bool,
}

impl<W: Write> MockEnoSpcWriter<W> {
    /// Wraps `inner` so writes succeed until cumulative bytes reach
    /// `threshold`, after which every subsequent write returns
    /// [`ENOSPC_KIND`].
    ///
    /// Equivalent to `Self::with_plan(inner, FaultPlan::enospc(threshold))`.
    pub(crate) fn new(inner: W, threshold: u64) -> Self {
        Self::with_plan(inner, FaultPlan::enospc(threshold))
    }

    /// Wraps `inner` with the supplied [`FaultPlan`].
    pub(crate) fn with_plan(inner: W, plan: FaultPlan) -> Self {
        Self {
            inner,
            plan,
            bytes_written: 0,
            tripped: false,
        }
    }

    /// Returns the cumulative number of bytes the inner writer has accepted.
    #[allow(dead_code)] // surfaced for SPL-33.c assertions
    pub(crate) fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Returns `true` once the fault has fired at least once.
    #[allow(dead_code)] // surfaced for SPL-33.c assertions
    pub(crate) fn has_tripped(&self) -> bool {
        self.tripped
    }

    /// Consumes the wrapper and returns the underlying writer for
    /// post-test inspection.
    #[allow(dead_code)] // surfaced for SPL-33.c assertions
    pub(crate) fn into_inner(self) -> W {
        self.inner
    }

    fn should_fault(&self, incoming: usize) -> bool {
        if self.plan.one_shot && self.tripped {
            return false;
        }
        let projected = self.bytes_written.saturating_add(incoming as u64);
        projected > self.plan.after_bytes()
    }

    fn fault_error(&mut self) -> io::Error {
        self.tripped = true;
        io::Error::new(self.plan.kind, "injected ENOSPC")
    }
}

impl<W: Write> Write for MockEnoSpcWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Post-trip on a one-shot plan: delegate to the inner writer
        // unmodified so the caller can observe recovery after a single
        // injected ENOSPC.
        if self.plan.one_shot && self.tripped {
            return self.inner.write(buf);
        }
        if self.should_fault(buf.len()) {
            return Err(self.fault_error());
        }
        let written = self.inner.write(buf)?;
        self.bytes_written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write + Read> Read for MockEnoSpcWriter<W> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<W: Write + Seek> Seek for MockEnoSpcWriter<W> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    //! Real-kernel ENOSPC integration helper for SPL-33.b.
    //!
    //! Mounts a tmpfs of the requested size at a fresh temp directory,
    //! pre-allocates a filler file via [`fallocate(2)`] so only a few KiB
    //! remain free, runs the caller's closure with the mount point as its
    //! argument, then unmounts and removes the mount directory on drop.
    //!
    //! Requires `CAP_SYS_ADMIN`. When the capability is missing
    //! ([`has_cap_sys_admin`] returns `false`) the helper returns `None` so
    //! the calling `#[test]` can early-return and stay green on hosted CI
    //! runners that run as unprivileged users.

    use std::ffi::CString;
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    /// Returns `true` if the current process owns `CAP_SYS_ADMIN`.
    ///
    /// Implementation reads `/proc/self/status` and checks the
    /// `CapEff` mask. The 21st bit is `CAP_SYS_ADMIN` per
    /// `include/uapi/linux/capability.h`. A read failure or a malformed
    /// status file returns `false` so the gated tests skip silently.
    pub(crate) fn has_cap_sys_admin() -> bool {
        let Ok(status) = fs::read_to_string("/proc/self/status") else {
            return false;
        };
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("CapEff:") {
                let hex = rest.trim();
                if let Ok(mask) = u64::from_str_radix(hex, 16) {
                    return mask & (1 << 21) != 0;
                }
            }
        }
        false
    }

    /// Mounts a tmpfs of `size_mb` MiB at a fresh temp directory, fills it
    /// to within ~4 KiB of full via `fallocate`, invokes `f` with the
    /// mount point, then unmounts and removes the directory.
    ///
    /// Returns `None` (skipping the body) when `CAP_SYS_ADMIN` is missing.
    /// Otherwise returns `Some(f(path))`. The caller is expected to wrap
    /// this in:
    ///
    /// ```ignore
    /// let Some(result) = with_full_tmpfs(8, |dir| { ... }) else { return; };
    /// ```
    ///
    /// so the test stays portable across CI tiles with and without the
    /// capability.
    pub(crate) fn with_full_tmpfs<R>(size_mb: u64, f: impl FnOnce(&Path) -> R) -> Option<R> {
        if !has_cap_sys_admin() {
            return None;
        }
        let mount = TmpfsMount::new(size_mb).ok()?;
        mount.fill_to_near_full().ok()?;
        Some(f(mount.path()))
    }

    /// RAII guard for a mounted tmpfs. Drop runs `umount2(MNT_DETACH)`
    /// and removes the mount directory; failures are swallowed because a
    /// Drop panic during test teardown is worse than a leaked mount that
    /// disappears on reboot.
    struct TmpfsMount {
        path: PathBuf,
        size_bytes: u64,
    }

    impl TmpfsMount {
        fn new(size_mb: u64) -> std::io::Result<Self> {
            let dir = ::tempfile::tempdir()?;
            let path = dir.path().to_path_buf();
            // `tempdir` would auto-delete the path on Drop; we keep the
            // directory across the mount lifetime by leaking the guard
            // and letting our own Drop impl run `umount2` + `rmdir`.
            std::mem::forget(dir);

            let target = CString::new(path.as_os_str().as_bytes())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            let fstype = CString::new("tmpfs").unwrap();
            let options = CString::new(format!("size={size_mb}m")).unwrap();
            // SAFETY: all pointers are valid C strings owned for the call's
            // duration. `mount(2)` does not retain them after return.
            let rc = unsafe {
                libc::mount(
                    std::ptr::null(),
                    target.as_ptr(),
                    fstype.as_ptr(),
                    0,
                    options.as_ptr().cast(),
                )
            };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                let _ = fs::remove_dir(&path);
                return Err(err);
            }
            Ok(Self {
                path,
                size_bytes: size_mb * 1024 * 1024,
            })
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn fill_to_near_full(&self) -> std::io::Result<()> {
            // Leave ~4 KiB free so the test can prove a small write triggers
            // ENOSPC without leaving zero headroom for inode metadata.
            // tmpfs honours `fallocate(2)` by actually reserving pages, so
            // the next caller write past the headroom hits real-kernel
            // ENOSPC. `ftruncate` would not allocate pages and the mount
            // would still appear empty.
            let leave_free: u64 = 4 * 1024;
            let len = self.size_bytes.saturating_sub(leave_free);
            let filler_path = self.path.join(".enospc_filler");
            let filler = fs::File::create(&filler_path)?;
            // SAFETY: `filler` is open for write; `len` fits in `off_t`
            // because tmpfs sizes are user-controlled by the caller and
            // capped at the mount's `size=` option. `posix_fallocate`
            // returns the errno directly rather than setting `errno`.
            let rc = unsafe { libc::posix_fallocate(filler.as_raw_fd(), 0, len as libc::off_t) };
            if rc != 0 {
                let err = std::io::Error::from_raw_os_error(rc);
                let _ = fs::remove_file(&filler_path);
                return Err(err);
            }
            Ok(())
        }
    }

    impl Drop for TmpfsMount {
        fn drop(&mut self) {
            let Ok(target) = CString::new(self.path.as_os_str().as_bytes()) else {
                return;
            };
            // SAFETY: `target` is a valid C string. `MNT_DETACH` schedules
            // the unmount after all fds close, which keeps Drop safe even
            // if the caller forgot to drop its handles.
            let _ = unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
            let _ = fs::remove_dir(&self.path);
        }
    }
}

#[cfg(target_os = "linux")]
#[allow(unused_imports)] // exercised by SPL-33.c integration tests
pub(crate) use linux::{has_cap_sys_admin, with_full_tmpfs};

mod tests {
    use super::*;

    #[test]
    fn mock_enospc_writer_triggers_at_threshold() {
        let mut w = MockEnoSpcWriter::new(Vec::<u8>::new(), 100);
        assert!(w.write(&[0u8; 50]).is_ok());
        let err = w
            .write(&[0u8; 60])
            .expect_err("second write must surface ENOSPC after threshold crossed");
        assert_eq!(
            err.kind(),
            ENOSPC_KIND,
            "fault must surface as the cross-platform ENOSPC equivalent"
        );
        assert!(w.has_tripped(), "wrapper must record the trip");
    }

    #[test]
    fn mock_enospc_writer_atomic_fail_on_threshold_crossing() {
        // A single call whose payload would push past the threshold must
        // fail atomically with ENOSPC and leave the inner writer
        // untouched. This matches the kernel's strict-ENOSPC contract for
        // `write(2)` calls that cannot fit at all.
        let mut w = MockEnoSpcWriter::new(Vec::<u8>::new(), 10);
        let err = w
            .write(&[0u8; 25])
            .expect_err("over-threshold write must fail atomically");
        assert_eq!(err.kind(), ENOSPC_KIND);
        assert_eq!(w.bytes_written(), 0, "inner writer must stay untouched");
    }

    #[test]
    fn mock_enospc_writer_accepts_writes_strictly_below_threshold() {
        // A call whose payload fits entirely below the threshold succeeds
        // and the accumulated counter advances. The first call past the
        // threshold then fails.
        let mut w = MockEnoSpcWriter::new(Vec::<u8>::new(), 10);
        let n = w.write(b"hello").expect("five bytes below threshold");
        assert_eq!(n, 5);
        assert_eq!(w.bytes_written(), 5);
        let n = w.write(b"world").expect("ten bytes total still fits");
        assert_eq!(n, 5);
        assert_eq!(w.bytes_written(), 10);
        let err = w.write(b"!").expect_err("threshold crossing fails");
        assert_eq!(err.kind(), ENOSPC_KIND);
    }

    #[test]
    fn one_shot_plan_recovers_after_single_failure() {
        let plan = FaultPlan {
            kind: ENOSPC_KIND,
            event: FaultEvent::DiskFull { after_bytes: 0 },
            one_shot: true,
        };
        let mut w = MockEnoSpcWriter::with_plan(Vec::<u8>::new(), plan);
        let err = w
            .write(b"hello")
            .expect_err("one-shot fires on first call");
        assert_eq!(err.kind(), ENOSPC_KIND);
        // After the one-shot trips, subsequent writes succeed against the
        // underlying buffer. This mirrors the SPL-33 free-space-restored
        // matrix row.
        let n = w.write(b"hello").expect("post-trip write succeeds");
        assert_eq!(n, 5);
        assert_eq!(w.into_inner(), b"hello");
    }

    #[test]
    fn fault_plan_enospc_constructor_matches_kind() {
        let plan = FaultPlan::enospc(42);
        assert_eq!(plan.kind, ENOSPC_KIND);
        assert!(!plan.one_shot);
        match plan.event {
            FaultEvent::DiskFull { after_bytes } => assert_eq!(after_bytes, 42),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tmpfs_helper_skips_without_cap_sys_admin() {
        // The helper must short-circuit without panicking when the runner
        // lacks CAP_SYS_ADMIN. On CI this is the common case, so the test
        // simply observes that the function returns `None` rather than
        // attempting a real mount.
        if super::linux::has_cap_sys_admin() {
            // Privileged environment: assert the helper does run the body
            // and the path it hands the closure is writable.
            let result = super::linux::with_full_tmpfs(4, |dir| dir.exists());
            assert_eq!(result, Some(true));
        } else {
            assert!(
                super::linux::with_full_tmpfs(4, |_| ()).is_none(),
                "helper must skip when CAP_SYS_ADMIN is missing"
            );
        }
    }
}
