use std::fs::{File, OpenOptions};
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(windows)]
use std::collections::BTreeMap;
#[cfg(windows)]
use std::io::{Read, Seek, SeekFrom, Write};

#[cfg(windows)]
use fs2::FileExt;

use core::message::Role;
use core::rsync_error;

use crate::error::DaemonError;

use super::runtime::ModuleConnectionError;

/// Exit code used when daemon functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;

/// Width in bytes of a single connection slot in the lock file.
///
/// upstream: connection.c:38 `lock_range(fd, i*4, 4)` reserves four bytes per
/// slot, so slot `i` owns the byte range `[i*4, i*4+4)`. Matching the stride
/// keeps oc-rsync interoperable with an upstream `rsync --daemon` that shares
/// the same `lock file`.
#[cfg(unix)]
const SLOT_LEN: i64 = 4;

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
/// it locks. util1.c:632 `lock_range()` issues `fcntl(fd, F_SETLK, &lock)` with
/// `l_type = F_WRLCK`. On Windows, which has no equivalent lock-on-death
/// primitive, the limiter falls back to a counter serialised with `flock`.
pub(crate) struct ConnectionLimiter {
    path: PathBuf,
}

/// Creates a [`DaemonError`] for lock file open failures.
fn lock_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to open lock file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

impl ConnectionLimiter {
    /// Opens or creates the lock file at the given path.
    pub(crate) fn open(path: PathBuf) -> Result<Self, DaemonError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| lock_file_error(&path, error))?;
        }

        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| lock_file_error(&path, error))?;

        drop(file);

        Ok(Self { path })
    }

    /// Opens the lock file for read/write access.
    fn open_file(&self) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).open(&self.path)
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
    /// upstream: connection.c:37-40 - scan `[0, max_connections)` and return the
    /// first range that locks; a failed lock (`errno` unset) means capacity.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        module: &str,
        limit: NonZeroU32,
    ) -> Result<ConnectionLockGuard, ModuleConnectionError> {
        let _ = module;
        let file = self.open_file().map_err(ModuleConnectionError::io)?;

        for slot in 0..limit.get() {
            let lock = slot_lock(i64::from(slot) * SLOT_LEN);
            loop {
                match nix::fcntl::fcntl(&file, setlk_arg(&lock)) {
                    Ok(_) => return Ok(ConnectionLockGuard { _file: file }),
                    // The range is held by another connection or process; try the
                    // next slot. upstream: util1.c:642 - `fcntl` returns non-zero
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

        Err(ModuleConnectionError::Limit(limit))
    }
}

/// Builds an `F_WRLCK` description for the slot beginning at `offset`.
///
/// upstream: util1.c:634-640 sets `l_type = F_WRLCK`, `l_whence = SEEK_SET`,
/// `l_start = offset`, `l_len = len`.
#[cfg(unix)]
fn slot_lock(offset: i64) -> nix::libc::flock {
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
/// upstream: util1.c:642 uses `fcntl(fd, F_SETLK, &lock)`. Where OFD locks are
/// unavailable the in-process slot count is enforced by the caller's atomic
/// counter; `F_SETLK` still coordinates across separate daemon processes.
#[cfg(all(unix, not(target_os = "linux")))]
fn setlk_arg(lock: &nix::libc::flock) -> nix::fcntl::FcntlArg<'_> {
    nix::fcntl::FcntlArg::F_SETLK(lock)
}

/// RAII guard whose held file descriptor keeps the connection's slot locked.
///
/// Dropping the guard closes the descriptor, which releases the record lock.
/// The same release happens automatically if the process dies, so no explicit
/// decrement is required and a crash cannot leak the slot.
#[cfg(unix)]
pub(crate) struct ConnectionLockGuard {
    _file: File,
}

#[cfg(windows)]
impl ConnectionLimiter {
    /// Acquires a connection slot for the named module, enforcing the limit.
    ///
    /// Windows lacks a byte-range-lock-on-death primitive, so the count is kept
    /// in the lock file and serialised with an exclusive `flock`, with a RAII
    /// guard restoring the count on drop.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        module: &str,
        limit: NonZeroU32,
    ) -> Result<ConnectionLockGuard, ModuleConnectionError> {
        let mut file = self.open_file().map_err(ModuleConnectionError::io)?;
        file.lock_exclusive().map_err(ModuleConnectionError::io)?;

        let result = self.increment_count(&mut file, module, limit);
        drop(file);

        result.map(|_| ConnectionLockGuard {
            limiter: Arc::clone(self),
            module: module.to_owned(),
        })
    }

    /// Decrements the connection count for the named module.
    fn decrement(&self, module: &str) -> io::Result<()> {
        let mut file = self.open_file()?;
        file.lock_exclusive()?;
        let result = self.decrement_count(&mut file, module);
        drop(file);
        result
    }

    /// Increments the module count in the lock file, failing if the limit is reached.
    fn increment_count(
        &self,
        file: &mut File,
        module: &str,
        limit: NonZeroU32,
    ) -> Result<(), ModuleConnectionError> {
        let mut counts = self.read_counts(file)?;
        let current = counts.get(module).copied().unwrap_or(0);
        if current >= limit.get() {
            return Err(ModuleConnectionError::Limit(limit));
        }

        counts.insert(module.to_owned(), current.saturating_add(1));
        self.write_counts(file, &counts)
            .map_err(ModuleConnectionError::io)
    }

    /// Decrements the module count in the lock file, removing the entry when it reaches zero.
    fn decrement_count(&self, file: &mut File, module: &str) -> io::Result<()> {
        let mut counts = self.read_counts(file)?;
        if let Some(entry) = counts.get_mut(module) {
            if *entry > 1 {
                *entry -= 1;
            } else {
                counts.remove(module);
            }
        }

        self.write_counts(file, &counts)
    }

    /// Reads module connection counts from the lock file.
    fn read_counts(&self, file: &mut File) -> io::Result<BTreeMap<String, u32>> {
        file.seek(SeekFrom::Start(0))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let mut counts = BTreeMap::new();
        for line in contents.lines() {
            let mut parts = line.split_whitespace();
            if let (Some(name), Some(value)) = (parts.next(), parts.next())
                && let Ok(parsed) = value.parse::<u32>()
            {
                counts.insert(name.to_owned(), parsed);
            }
        }

        Ok(counts)
    }

    /// Writes module connection counts to the lock file, replacing existing content.
    fn write_counts(&self, file: &mut File, counts: &BTreeMap<String, u32>) -> io::Result<()> {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        for (module, value) in counts {
            if *value > 0 {
                writeln!(file, "{module} {value}")?;
            }
        }
        file.flush()
    }
}

/// RAII guard that decrements the lock file count on drop.
#[cfg(windows)]
pub(crate) struct ConnectionLockGuard {
    limiter: Arc<ConnectionLimiter>,
    module: String,
}

#[cfg(windows)]
impl Drop for ConnectionLockGuard {
    fn drop(&mut self) {
        let _ = self.limiter.decrement(&self.module);
    }
}
