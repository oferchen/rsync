use std::fs::File;
// Only the non-Unix arm of `open_lock_file` opens by path; Unix goes through
// the ownership walk.
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::max_connections::MaxConnections;
use super::runtime::ModuleConnectionError;

/// Width in bytes of a single connection slot in the lock file.
///
/// upstream: connection.c:40 `lock_range(fd, i*4, 4)` reserves four bytes per
/// slot, so slot `i` owns the byte range `[i*4, i*4+4)`. Matching the stride
/// keeps oc-rsync interoperable with an upstream `rsync --daemon` that shares
/// the same `lock file`. The Windows daemon uses the same layout.
const SLOT_LEN: u64 = 4;

/// File-based connection limiter for cross-process `max connections` enforcement.
///
/// Each accepted connection holds an advisory record lock on a distinct byte
/// range of the lock file for the lifetime of the connection. Because the
/// kernel releases fcntl locks when the owning process dies - on clean exit,
/// `SIGKILL`, or a `panic = "abort"` build - a crashed connection can never
/// leak a slot. A new connection scans the ranges `[0, max_connections)` and
/// claims the first one it can lock; if every range is held the module is at
/// capacity.
///
/// upstream: connection.c:26 `claim_connection()` opens the lock file and calls
/// `lock_range(fd, i*4, 4)` for each slot, returning success on the first range
/// it locks. util1.c:712-723 `lock_range()` issues `fcntl(fd, F_SETLK, &lock)` with
/// `l_type = F_WRLCK`. Windows takes the same ranges with `LockFileEx`
/// (`fast_io::range_lock`), which the system likewise releases when the
/// handle closes or the process dies.
pub(crate) struct ConnectionLimiter {
    path: PathBuf,
}

/// Mode applied when the lock file is created.
///
/// upstream: connection.c:35 `claim_connection()` passes `0600` to
/// `open_no_attacker_symlinks()`. The lock file records nothing secret, but the
/// mode is upstream's explicit choice rather than an inherited
/// `0666 & ~umask`, so a shared `lock file` is not left group- or
/// world-readable on a daemon running with a permissive umask.
///
/// Unix-only: the mode is consumed by the ownership-walk arm of
/// [`open_lock_file`], and Windows has no POSIX mode bits to pass.
#[cfg(unix)]
const LOCK_FILE_MODE: u32 = 0o600;

/// Opens the operator-named lock file, creating it if absent.
///
/// `lock file = PATH` is named by the operator in `oc-rsyncd.conf`, and the
/// daemon opens it as whatever user it is running as - typically root, and
/// before the per-module privilege drop. A symlink planted at any component
/// therefore redirects a privileged `O_CREAT` open to a file the operator never
/// named, and because the leaf need not exist yet a leaf-only `O_NOFOLLOW`
/// cannot reject it. The ownership walk refuses any component symlink not owned
/// by uid 0 or our euid.
///
/// upstream: connection.c:35 `claim_connection()` -
/// `open_no_attacker_symlinks(fname, O_RDWR|O_CREAT, 0600)`.
///
/// On Windows there is no `st_uid` to trust, so this degrades to a plain open,
/// matching how the rest of the tree gates `fast_io`'s operator-path helpers.
fn open_lock_file(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        fast_io::operator_open_rw_create(path, LOCK_FILE_MODE)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(crate::posix_path::resolve_daemon_path(path)?)
    }
}

impl ConnectionLimiter {
    /// Records the lock file path without touching the filesystem.
    ///
    /// Construction CANNOT fail. upstream has no build-time open at all: the
    /// lock file is opened inside `claim_connection()` (connection.c:35), and a
    /// failed open there is `return 0` - that one connection is refused while
    /// the daemon keeps listening. Propagating a build-time failure instead
    /// killed the daemon before `listen()` whenever an operator-named lock path
    /// was unopenable - including the refused-symlink case the upstream
    /// testsuite's `daemon-config-symlink` cell plants, which upstream serves.
    ///
    /// Nor does upstream create the file or its directory ahead of time: the
    /// `O_CREAT` open with mode 0600 in [`Self::acquire`] is the only create,
    /// and a missing parent directory is that connection's open failure.
    pub(crate) fn open(path: PathBuf) -> Self {
        Self { path }
    }

    /// Opens the lock file for read/write access.
    ///
    /// Creates it when absent, as upstream does on every `claim_connection()`:
    /// re-opening without `O_CREAT` would fail if the file were removed between
    /// [`ConnectionLimiter::open`] and the claim, and would resolve the operator
    /// path a second time outside the ownership walk.
    fn open_file(&self) -> io::Result<File> {
        open_lock_file(&self.path)
    }
}

#[cfg(unix)]
impl ConnectionLimiter {
    /// Acquires a connection slot for the named module, enforcing the limit.
    ///
    /// The `module` name is unused: upstream keys the slot pool by lock file,
    /// not by module, so modules sharing a `lock file` share its slots
    /// (rsyncd.conf(5), "lock file"). Isolation is obtained by giving a module
    /// its own `lock file`, exactly as upstream does.
    ///
    /// upstream: connection.c:35 opens the lock file before the scan, so an
    /// unopenable file is an open failure whatever the limit says.
    /// connection.c:39-42 then scans `[0, max_connections)` and returns the
    /// first range that locks; a failed lock (`errno` unset) means capacity. A
    /// disabling (negative) limit scans nothing and refuses here, matching the
    /// `close(fd); errno = 0; return 0` fall-through at connection.c:44-48.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        module: &str,
        limit: MaxConnections,
    ) -> Result<ConnectionLockGuard, ModuleConnectionError> {
        let _ = module;
        let file = self.open_file().map_err(ModuleConnectionError::io)?;

        for slot in 0..limit.slot_count() {
            let lock = slot_lock(u64::from(slot) * SLOT_LEN);
            loop {
                match nix::fcntl::fcntl(&file, setlk_arg(&lock)) {
                    Ok(_) => return Ok(ConnectionLockGuard { _file: file }),
                    // The range is held by another connection or process; try the
                    // next slot. upstream: util1.c:722 - `fcntl` returns non-zero
                    // with EACCES/EAGAIN when the lock is contended.
                    Err(nix::errno::Errno::EACCES | nix::errno::Errno::EAGAIN) => break,
                    // A signal interrupted the lock request before it resolved;
                    // retry the same slot rather than treating it as an error or
                    // as spurious capacity.
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(err) => {
                        return Err(ModuleConnectionError::io(io::Error::from_raw_os_error(
                            err as i32,
                        )));
                    }
                }
            }
        }

        // Every slot in `[0, slot_count)` was held, so that count is exactly
        // how many connections the lock file - the cross-process owner of the
        // decision - observes.
        Err(ModuleConnectionError::limit(limit, limit.slot_count()))
    }
}

/// Builds an `F_WRLCK` description for the slot beginning at `offset`.
///
/// upstream: util1.c:716-722 sets `l_type = F_WRLCK`, `l_whence = SEEK_SET`,
/// `l_start = offset`, `l_len = len`.
#[cfg(unix)]
fn slot_lock(offset: u64) -> nix::libc::flock {
    nix::libc::flock {
        l_type: nix::libc::F_WRLCK as nix::libc::c_short,
        l_whence: nix::libc::SEEK_SET as nix::libc::c_short,
        l_start: offset as nix::libc::off_t,
        l_len: SLOT_LEN as nix::libc::off_t,
        l_pid: 0,
    }
}

/// Selects the non-blocking `F_SETLK` command for the current platform.
///
/// Linux prefers open file description locks (`F_OFD_SETLK`) so that two
/// connection threads in this single daemon process - which upstream would fork
/// into separate processes - contend on distinct slots rather than sharing one
/// process-owned lock. OFD locks conflict with traditional POSIX locks, so
/// coordination with an upstream `rsync --daemon` sharing the file is retained.
#[cfg(target_os = "linux")]
fn setlk_arg(lock: &nix::libc::flock) -> nix::fcntl::FcntlArg<'_> {
    nix::fcntl::FcntlArg::F_OFD_SETLK(lock)
}

/// Selects the non-blocking `F_SETLK` command on non-Linux unix platforms.
///
/// upstream: util1.c:722 uses `fcntl(fd, F_SETLK, &lock)`. Where OFD locks are
/// unavailable the in-process slot count is enforced by the caller's atomic
/// counter; `F_SETLK` still coordinates across separate daemon processes.
#[cfg(all(unix, not(target_os = "linux")))]
fn setlk_arg(lock: &nix::libc::flock) -> nix::fcntl::FcntlArg<'_> {
    nix::fcntl::FcntlArg::F_SETLK(lock)
}

/// RAII guard whose held file keeps the connection's slot locked.
///
/// Dropping the guard closes the file, which releases the range lock. The same
/// release happens automatically if the process dies, so no explicit decrement
/// is required and a crash cannot leak the slot.
pub(crate) struct ConnectionLockGuard {
    _file: File,
}

#[cfg(windows)]
impl ConnectionLimiter {
    /// Acquires a connection slot, enforcing the limit across processes.
    ///
    /// Mirrors the Unix scan: the lock file is opened first for every non-zero
    /// limit (connection.c:35), then `[0, max_connections)` is scanned for the
    /// first 4-byte range that locks (connection.c:39-42). `LockFileEx` locks
    /// belong to the file handle, so concurrent sessions - each a separate
    /// process on Windows - contend exactly as upstream's forked children do,
    /// and a killed session frees its slot. The `module` name is unused for
    /// the same reason as on Unix: slots are keyed by lock file.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        module: &str,
        limit: MaxConnections,
    ) -> Result<ConnectionLockGuard, ModuleConnectionError> {
        let _ = module;
        let file = self.open_file().map_err(ModuleConnectionError::io)?;

        for slot in 0..limit.slot_count() {
            let offset = u64::from(slot) * SLOT_LEN;
            if fast_io::range_lock::try_lock_exclusive_range(&file, offset, SLOT_LEN)
                .map_err(ModuleConnectionError::io)?
            {
                return Ok(ConnectionLockGuard { _file: file });
            }
        }

        Err(ModuleConnectionError::limit(limit, limit.slot_count()))
    }
}
