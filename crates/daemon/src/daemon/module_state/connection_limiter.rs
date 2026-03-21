use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;

use core::message::Role;
use core::rsync_error;

use crate::error::DaemonError;

use super::runtime::ModuleConnectionError;

/// Exit code used when daemon functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;

/// File-based connection limiter for cross-process max_connections enforcement.
///
/// Uses a shared lock file to coordinate connection counts across multiple
/// daemon processes. Each module's active count is stored as a line in the file.
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

    /// Acquires a connection slot for the named module, enforcing the limit.
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

    /// Opens the lock file for read/write access.
    fn open_file(&self) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).open(&self.path)
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
pub(crate) struct ConnectionLockGuard {
    limiter: Arc<ConnectionLimiter>,
    module: String,
}

impl Drop for ConnectionLockGuard {
    fn drop(&mut self) {
        let _ = self.limiter.decrement(&self.module);
    }
}
