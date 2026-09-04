//! Cleanup coordination for temporary files and resources.
//!
//! Provides a global cleanup manager that tracks temporary files, partial
//! transfers, and other resources that need cleanup on shutdown or error.
//! Signal handlers and RAII guards in the `transfer` and `core` crates use
//! this registry to ensure stale temp files are removed on abnormal exit.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Global cleanup manager instance.
static CLEANUP_MANAGER: OnceLock<Mutex<CleanupManagerState>> = OnceLock::new();

/// An in-progress temp file and where its partial data should end up if the
/// transfer is cut short. Mirrors upstream's `cleanup_fname`/`cleanup_new_fname`
/// pair (`cleanup.c:cleanup_set()`): `temp` is the `.name.XXXXXX` staging file,
/// and `partial_dest` is the destination the partial is moved to on interrupt
/// (the real file for `--partial`, or the partial-dir entry for
/// `--partial-dir`). `None` means "no partial kept" - just unlink the temp.
#[derive(Clone, Debug)]
struct PartialEntry {
    temp: PathBuf,
    partial_dest: Option<PathBuf>,
    tweak_mtime: bool,
}

/// Creates a `--partial-dir` with upstream's private 0700 mode.
///
/// upstream: `util1.c:handle_partial_dir()` - `do_mkdir_at(dir, 0700)`. The mode
/// is load-bearing rather than incidental: the partial dir holds a complete copy
/// of the file for the length of the transfer (and across runs, for a reserved
/// absolute dir), so a umask-derived 0755 exposes that content to every local
/// user. This is the single owner of the rule for both the abort path
/// ([`finalize_partial`]) and the `--delay-updates` staging path.
///
/// On unix *both* path decisions go through the ownership walk, because
/// upstream wraps both in `operator_path_resolve = 1`: the reuse probe
/// (`do_lstat_at`, `util1.c:1521`) and the create (`do_mkdir_at`,
/// `util1.c:1529`). An absolute `--partial-dir` names a location outside the
/// transfer tree, so a foreign-owned symlink planted at any component would
/// otherwise redirect the staged file - a complete copy of the source - out of
/// the tree. Probing with `Path::is_dir()` would defeat the walk on exactly the
/// case an operator hits most: a reserved absolute partial dir that already
/// exists, where the create is never reached at all.
///
/// Any missing ancestor is created first, matching what the abort path has
/// always done; upstream mkdirs only the final component and fails when the
/// parent is absent. Each level goes through its own walk.
pub fn create_partial_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        // `""`, `/`, `.` and `..` name a directory that already exists and have
        // no leaf for the walk to resolve.
        if dir.file_name().is_none() {
            return Ok(());
        }
        // upstream: util1.c:1521 - `do_lstat_at(dir, &st)` under
        // `operator_path_resolve`, so the reuse probe is confined by the same
        // rule as the create. A refusal propagates rather than degrading to an
        // unconfined create.
        match fast_io::operator_symlink_metadata(dir) {
            // upstream: util1.c:1522 - `statret == 0 && S_ISDIR` skips the
            // mkdir and the dir is reused as it stands.
            Ok(metadata) if metadata.is_dir() => return Ok(()),
            // Something that is not a directory occupies the name. Upstream
            // unlinks it (util1.c:1523); oc has never done that, and the
            // `operator_mkdir` below keeps today's behaviour of reporting the
            // resulting `EEXIST` as success. Left unchanged here on purpose:
            // this function also creates ancestors upstream never touches, so
            // an unlink placed here would delete beyond the leaf.
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = dir.parent()
                    && !parent.as_os_str().is_empty()
                {
                    create_partial_dir(parent)?;
                }
            }
            Err(error) => return Err(error),
        }
        fast_io::operator_mkdir(dir, 0o700)
    }
    #[cfg(not(unix))]
    {
        std::fs::DirBuilder::new().recursive(true).create(dir)
    }
}
/// Removes a non-directory occupying the `--partial-dir` name so the staging
/// directory can be created in its place.
///
/// A no-op when the name is absent or already a directory; the caller creates
/// it in the first case and reuses it in the second.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/util1.c:1521-1528` `handle_partial_dir()` - `do_lstat_at`
///   then, when the entry exists and is not a directory,
///   `do_unlink_at(dir) < 0` aborts, otherwise `do_mkdir_at` runs against the
///   cleared name.
///
/// Only the FINAL component is cleared. Upstream's `handle_partial_dir` names
/// exactly one directory, so an obstruction standing where an *ancestor*
/// belongs is not something upstream removes and is not removed here either -
/// it surfaces as the caller's create error.
///
/// Both the probe and the removal run through the ownership walk, exactly as
/// upstream wraps its `do_lstat_at`/`do_unlink_at` pair in
/// `operator_path_resolve` (util1.c:1516-1527). That is what makes clearing a
/// SYMLINK safe: the walk resolves the parent chain and the leaf is handed to
/// `unlinkat` as a single component, so the link is removed as the
/// non-directory it is and is never followed to whatever it points at.
///
/// ⚠ A symlink here is exactly the shape that must NOT be left standing. A
/// peer-supplied `--partial-dir` naming a symlink out of the served tree used
/// to be accepted as "the directory is already there" - `mkdirat` reports
/// `EEXIST` for a symlink and every caller treats `EEXIST` as success - after
/// which the staging rename followed the link and wrote outside the module.
/// MEASURED against a daemon push with `--delay-updates --partial-dir=/blink`
/// where `blink` pointed outside: the outside file was replaced and then moved
/// away by the delayed-updates sweep. Upstream never reaches that state because
/// it clears the obstruction first, and fails the file outright if it cannot.
///
/// # Errors
///
/// Surfaces the walk's refusal, the `lstat` error for anything other than "not
/// found", and any `unlink` error. A failure here is fatal to the file: upstream
/// returns 0 from `handle_partial_dir()`, and `receiver.c:1302-1306` then
/// discards the received temp rather than staging around the obstruction.
pub fn clear_partial_dir_obstruction(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        match fast_io::operator_symlink_metadata(dir) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            // upstream: util1.c:1522-1527 - `statret == 0 && !S_ISDIR(st.st_mode)`
            // covers a symlink and a regular file alike; both are unlinked, and a
            // failure to unlink returns 0 (failure) rather than proceeding.
            Ok(_) => fast_io::operator_unlink(dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    {
        match std::fs::symlink_metadata(dir) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(_) => std::fs::remove_file(dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Removes an emptied `--partial-dir` once its staged file has been committed.
///
/// upstream: `util1.c:1501-1535 handle_partial_dir(fname, PDIR_DELETE)`, whose
/// delete half opens with `if (!create && *partial_dir == '/') return 1;`. An
/// ABSOLUTE `--partial-dir` is therefore never rmdir'd: it is operator-named,
/// it is reserved across runs, and it generally exists before the transfer
/// starts. Only a relative one - created beside each destination file and
/// belonging to that file's transfer - is swept away once emptied. Upstream
/// keeps both halves of the rule in one function, which is why this sits beside
/// [`create_partial_dir`] instead of being open-coded at each call site: the
/// rule had been written at one of oc's two removal sites and forgotten at the
/// other, and the forgotten one destroyed a pre-existing absolute partial-dir.
///
/// `partial_dir` is the configured option value - the string upstream tests
/// with `*partial_dir == '/'`. `None` means no `--partial-dir` was given, in
/// which case `--delay-updates` stages through upstream's implicit
/// `.~tmp~` (`options.c:347,2564` assign the literal `tmp_partialdir`), which is
/// relative and so always removable.
///
/// `staged_file` is the entry inside the directory; its parent is what gets
/// removed, exactly as upstream derives the directory by truncating
/// `partial_fname` at its last `'/'`.
///
/// Best-effort, like upstream's unchecked `do_rmdir_at`: a partial-dir that
/// still holds another file stays put.
pub fn remove_partial_dir(partial_dir: Option<&Path>, staged_file: &Path) {
    if partial_dir.is_some_and(Path::is_absolute) {
        return;
    }
    if let Some(dir) = staged_file.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}
/// Moves an interrupted temp file to its partial destination, or removes it.
///
/// upstream: `cleanup.c:exit_cleanup()` calls `finish_transfer()` to rename the
/// temp onto `cleanup_new_fname` (creating the partial dir first via
/// `handle_partial_dir(PDIR_CREATE)`), tweaking the modtime to epoch 0 when the
/// partial lands on the real destination file so `--update` will not skip it.
/// With no partial destination it unlinks the temp (`do_unlink_at`).
///
/// Creating the partial dir is a PRECONDITION of the rename, not a best-effort
/// prelude: upstream guards `finish_transfer()` on it in both places
/// (`cleanup.c:168` has the call inside a `&&`; `receiver.c:1302-1306` reports
/// "Unable to create partial-dir for %s -- discarding %s" and `do_unlink_at`s
/// the temp). Renaming anyway would undo the ownership walk's refusal - the
/// rename resolves the same path with plain libc and lands the file exactly
/// where the walk declined to create the directory.
pub fn finalize_partial(temp: &Path, partial_dest: Option<&Path>, tweak_mtime: bool) {
    match partial_dest {
        Some(dest) => {
            if let Some(parent) = dest.parent()
                && create_partial_dir(parent).is_err()
            {
                // upstream: receiver.c:1306 `do_unlink_at(fnametmp)` - the
                // completed file is discarded rather than retained somewhere
                // the operator did not name.
                let _ = std::fs::remove_file(temp);
                return;
            }
            // Best-effort: an already-committed transfer leaves no temp, so a
            // failed rename here is expected and harmless.
            if std::fs::rename(temp, dest).is_ok() && tweak_mtime {
                let epoch = std::time::SystemTime::UNIX_EPOCH;
                let times = std::fs::FileTimes::new().set_modified(epoch);
                if let Ok(file) = std::fs::File::options().write(true).open(dest) {
                    let _ = file.set_times(times);
                }
            }
        }
        None => {
            let _ = std::fs::remove_file(temp);
        }
    }
}

/// Cleanup manager for tracking and cleaning up temporary resources.
///
/// This type provides a global registry for temporary files and resources
/// that should be cleaned up on shutdown or error. It works with signal
/// handlers and RAII guards to ensure stale temp files are removed.
///
/// # Thread Safety
///
/// All methods are thread-safe and can be called from multiple threads
/// simultaneously. The internal state is protected by a mutex.
///
/// # Examples
///
/// ```
/// use engine::CleanupManager;
/// use std::path::PathBuf;
///
/// // Register a temp file for cleanup
/// let temp_file = PathBuf::from("/tmp/rsync.12345.tmp");
/// CleanupManager::global().register_temp_file(temp_file.clone());
///
/// // Do work...
///
/// // If successful, unregister so it's not cleaned up
/// CleanupManager::global().unregister_temp_file(&temp_file);
///
/// // Or if there's an error, cleanup all registered files
/// // CleanupManager::global().cleanup();
/// ```
#[derive(Debug)]
pub struct CleanupManager;

impl CleanupManager {
    /// Returns a reference to the global cleanup manager.
    ///
    /// This is the primary entry point for cleanup operations.
    #[must_use]
    pub fn global() -> &'static Self {
        let _ = CLEANUP_MANAGER.get_or_init(|| Mutex::new(CleanupManagerState::new()));
        &CLEANUP_MANAGER_INSTANCE
    }

    /// Registers a temporary file for cleanup.
    ///
    /// The file will be deleted when [`cleanup`](Self::cleanup) or
    /// [`cleanup_temp_files`](Self::cleanup_temp_files) is called,
    /// unless it is unregistered first.
    pub fn register_temp_file(&self, path: PathBuf) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.insert(path);
            }
        }
    }

    /// Unregisters a temporary file from cleanup.
    ///
    /// Call this when a temporary file has been successfully committed
    /// (renamed to its final destination) and should not be deleted
    /// during cleanup.
    pub fn unregister_temp_file(&self, path: &Path) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.remove(path);
            }
        }
    }

    /// Registers a cleanup callback to run on shutdown.
    ///
    /// The callback will be executed when [`cleanup`](Self::cleanup) is called.
    /// Callbacks are run in reverse order of registration (LIFO).
    pub fn register_cleanup(&self, callback: Box<dyn FnOnce() + Send>) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup_callbacks.push(callback);
            }
        }
    }

    /// Performs cleanup of all registered resources.
    ///
    /// This method:
    /// 1. Runs all registered cleanup callbacks in reverse order (LIFO)
    /// 2. Deletes all registered temporary files
    /// 3. Clears all registered resources
    ///
    /// Cleanup errors are logged but do not prevent other cleanup from proceeding.
    pub fn cleanup(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup();
            }
        }
    }

    /// Cleans up only the registered temporary files.
    ///
    /// Similar to [`cleanup`](Self::cleanup) but only removes temporary files,
    /// without running cleanup callbacks.
    pub fn cleanup_temp_files(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup_temp_files();
            }
        }
    }

    /// Returns the number of registered temporary files.
    ///
    /// Primarily useful for testing and diagnostics.
    #[must_use]
    pub fn temp_file_count(&self) -> usize {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(state) = state.lock() {
                return state.temp_files.len();
            }
        }
        0
    }

    /// Registers an in-progress temp file and its partial destination.
    ///
    /// Called when a `--partial`/`--partial-dir` staging file is opened so that
    /// a signal handler's abort path can finalise it even if the owning thread
    /// never returns to run its RAII guard. `partial_dest` is `None` for a
    /// non-partial transfer (the temp is simply unlinked on abort).
    pub fn register_partial(
        &self,
        temp: PathBuf,
        partial_dest: Option<PathBuf>,
        tweak_mtime: bool,
    ) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.partials.retain(|entry| entry.temp != temp);
                state.partials.push(PartialEntry {
                    temp,
                    partial_dest,
                    tweak_mtime,
                });
            }
        }
    }

    /// Removes a temp file from the partial registry after its guard committed
    /// or already finalised it.
    pub fn unregister_partial(&self, temp: &Path) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.partials.retain(|entry| entry.temp != temp);
            }
        }
    }

    /// Finalises every registered in-progress temp: moves each onto its partial
    /// destination (or unlinks it), then clears the registry. Invoked from the
    /// abort path (a second interrupt signal) that cannot wait for graceful
    /// unwinding. upstream: `cleanup.c:exit_cleanup()` on `RERR_SIGNAL`.
    pub fn finalize_partials(&self) {
        let entries = {
            let Some(state) = CLEANUP_MANAGER.get() else {
                return;
            };
            let Ok(mut state) = state.lock() else {
                return;
            };
            std::mem::take(&mut state.partials)
        };
        for entry in entries {
            finalize_partial(
                &entry.temp,
                entry.partial_dest.as_deref(),
                entry.tweak_mtime,
            );
        }
    }

    /// Clears all registered resources without performing cleanup.
    ///
    /// Primarily useful for testing.
    #[doc(hidden)]
    pub fn reset_for_testing(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.clear();
                state.cleanup_callbacks.clear();
                state.partials.clear();
            }
        }
    }
}

/// Singleton instance of the cleanup manager.
static CLEANUP_MANAGER_INSTANCE: CleanupManager = CleanupManager;

/// Internal state for the cleanup manager.
struct CleanupManagerState {
    temp_files: HashSet<PathBuf>,
    cleanup_callbacks: Vec<Box<dyn FnOnce() + Send>>,
    partials: Vec<PartialEntry>,
}

impl CleanupManagerState {
    fn new() -> Self {
        Self {
            temp_files: HashSet::new(),
            cleanup_callbacks: Vec::new(),
            partials: Vec::new(),
        }
    }

    fn cleanup(&mut self) {
        while let Some(callback) = self.cleanup_callbacks.pop() {
            callback();
        }

        self.cleanup_temp_files();
    }

    fn cleanup_temp_files(&mut self) {
        for path in &self.temp_files {
            let _ = std::fs::remove_file(path);
        }
        self.temp_files.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use tempfile::tempdir;

    // Global lock to serialize tests that use the global CleanupManager
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    #[test]
    fn register_and_unregister_temp_file() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let path = PathBuf::from("/tmp/test_register_unregister.tmp");

        manager.register_temp_file(path.clone());
        assert_eq!(manager.temp_file_count(), 1);

        manager.unregister_temp_file(&path);
        assert_eq!(manager.temp_file_count(), 0);
    }

    #[test]
    fn cleanup_temp_files_removes_files() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("tempdir");
        let path1 = dir.path().join("test1_cleanup.tmp");
        let path2 = dir.path().join("test2_cleanup.tmp");

        fs::write(&path1, b"data1").expect("write file 1");
        fs::write(&path2, b"data2").expect("write file 2");

        manager.register_temp_file(path1.clone());
        manager.register_temp_file(path2.clone());

        assert!(path1.exists());
        assert!(path2.exists());
        assert_eq!(manager.temp_file_count(), 2);

        manager.cleanup_temp_files();

        assert!(!path1.exists());
        assert!(!path2.exists());
        assert_eq!(manager.temp_file_count(), 0);
    }

    #[test]
    fn cleanup_temp_files_ignores_nonexistent() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let path = PathBuf::from("/tmp/nonexistent_test.tmp");
        manager.register_temp_file(path);

        manager.cleanup_temp_files();
        assert_eq!(manager.temp_file_count(), 0);
    }

    #[test]
    fn cleanup_callbacks_run_in_reverse_order() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let order = Arc::new(Mutex::new(Vec::new()));

        let order1 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || {
            order1.lock().unwrap().push(1);
        }));

        let order2 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || {
            order2.lock().unwrap().push(2);
        }));

        let order3 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || {
            order3.lock().unwrap().push(3);
        }));

        manager.cleanup();

        let final_order = order.lock().unwrap();
        assert_eq!(*final_order, vec![3, 2, 1]);
    }

    #[test]
    fn cleanup_runs_callbacks_and_removes_files() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test_cleanup_all.tmp");
        fs::write(&path, b"data").expect("write file");

        manager.register_temp_file(path.clone());

        let callback_ran = Arc::new(AtomicBool::new(false));
        let callback_flag = Arc::clone(&callback_ran);
        manager.register_cleanup(Box::new(move || {
            callback_flag.store(true, Ordering::SeqCst);
        }));

        manager.cleanup();

        assert!(callback_ran.load(Ordering::SeqCst));
        assert!(!path.exists());
        assert_eq!(manager.temp_file_count(), 0);
    }

    #[test]
    fn unregister_prevents_cleanup() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test_unregister.tmp");
        fs::write(&path, b"data").expect("write file");

        manager.register_temp_file(path.clone());
        manager.unregister_temp_file(&path);

        manager.cleanup_temp_files();

        assert!(path.exists(), "unregistered file must survive cleanup");
    }

    #[test]
    fn multiple_registrations_of_same_file() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let path = PathBuf::from("/tmp/test_multiple.tmp");

        manager.register_temp_file(path.clone());
        manager.register_temp_file(path.clone());
        manager.register_temp_file(path);

        assert_eq!(manager.temp_file_count(), 1, "HashSet deduplicates");
    }

    /// Builds the shape the `--partial-dir` reuse probe has to decide on: an
    /// absolute partial dir `<plant>/opd/sub` whose parent component `opd` is a
    /// symlink to an out-of-tree directory, with `sub` ALREADY PRESENT inside
    /// it. Returns `(partial_dir, planted_link, out_of_tree_leaf)`.
    ///
    /// The leaf existing is the whole point: it is the arm a probe that stats
    /// through the symlink answers "already a directory, nothing to do" - never
    /// reaching the create, and therefore never reaching the ownership walk.
    #[cfg(unix)]
    fn plant_reused_partial_dir(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let outside = base.join("outside");
        let leaf = outside.join("sub");
        fs::create_dir_all(&leaf).expect("create the out-of-tree leaf");
        let plant = base.join("plant");
        fs::create_dir(&plant).expect("create the plant dir");
        let link = plant.join("opd");
        std::os::unix::fs::symlink(&outside, &link).expect("plant the parent symlink");
        (link.join("sub"), link, leaf)
    }

    /// The operator's own symlink is followed, so an existing partial dir
    /// reached through it is reused. This is the half that must keep working:
    /// `/backup -> /mnt/disk` is the ordinary administrative layout, and
    /// refusing every parent symlink would break it.
    ///
    /// upstream: `syscall.c:406` - uid 0 or our euid is trusted and followed.
    #[cfg(unix)]
    #[test]
    fn an_existing_partial_dir_behind_the_operators_own_parent_symlink_is_reused() {
        let base = tempdir().expect("tempdir");
        let (partial_dir, _link, leaf) = plant_reused_partial_dir(base.path());

        create_partial_dir(&partial_dir).expect("the operator's own symlink must be followed");
        assert!(leaf.is_dir(), "the existing leaf is reused, not replaced");
    }

    /// The same shape with the symlink owned by someone else must be refused,
    /// even though the leaf already exists.
    ///
    /// upstream: `util1.c:1521` `handle_partial_dir()` runs its `do_lstat_at()`
    /// reuse probe under `operator_path_resolve = 1`, i.e. through the same
    /// ownership walk as the `do_mkdir_at()` beneath it. Probing with a
    /// following stat would answer this case before the walk ever ran, and the
    /// staged file - a complete copy of the source - would land in
    /// `outside/sub`.
    ///
    /// Planting a symlink owned by a foreign uid needs root, which is why
    /// upstream's own `operator-path-partial-dir [reuse]` cell only exercises
    /// this on its root leg. The companion above carries the other direction at
    /// any uid.
    #[cfg(unix)]
    #[test]
    fn an_existing_partial_dir_behind_a_foreign_owned_parent_symlink_is_refused() {
        if rustix::process::geteuid().as_raw() != 0 {
            eprintln!(
                "SKIPPED an_existing_partial_dir_behind_a_foreign_owned_parent_symlink_is_refused: \
                 planting a symlink owned by a foreign uid requires root"
            );
            return;
        }
        // An arbitrary uid that is neither 0 nor our euid; `lchown` accepts it
        // whether or not an account exists, and the walk compares numbers.
        const ATTACKER_UID: u32 = 12345;

        let base = tempdir().expect("tempdir");
        let (partial_dir, link, leaf) = plant_reused_partial_dir(base.path());
        std::os::unix::fs::lchown(&link, Some(ATTACKER_UID), Some(ATTACKER_UID))
            .expect("give the planted symlink to the attacker");

        create_partial_dir(&partial_dir)
            .expect_err("a foreign-owned parent symlink must be refused, leaf present or not");
        assert!(
            leaf.is_dir(),
            "the refusal must not have disturbed the out-of-tree leaf"
        );
    }

    #[test]
    fn global_returns_same_instance() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager1 = CleanupManager::global();
        let manager2 = CleanupManager::global();

        manager1.reset_for_testing();
        manager1.register_temp_file(PathBuf::from("/tmp/test_global.tmp"));

        assert_eq!(manager2.temp_file_count(), 1);
    }

    /// A SYMLINK standing at the `--partial-dir` name is REMOVED, and what it
    /// points at is left alone.
    ///
    /// This is the security shape: a peer-supplied `--partial-dir` naming a
    /// symlink out of the served tree. Leaving the link standing let `mkdirat`
    /// report `EEXIST`, every caller read that as "the directory is already
    /// there", and the staging rename then wrote THROUGH the link - measured
    /// destroying a file outside a daemon module.
    ///
    /// Both halves are asserted deliberately. "The link is gone" alone would
    /// also hold for an implementation that followed the link and deleted its
    /// TARGET, which is the opposite of the fix; and "the target survives"
    /// alone would hold for the old pass-through that removed nothing.
    ///
    /// upstream: `util1.c:1522-1527` - `statret == 0 && !S_ISDIR(st.st_mode)`
    /// unlinks, and a failure to unlink fails the whole call.
    #[cfg(unix)]
    #[test]
    fn clearing_the_partial_dir_removes_a_symlink_without_touching_its_target() {
        let dir = tempdir().expect("tempdir");
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).expect("mkdir outside");
        let victim = outside.join("keep-me");
        fs::write(&victim, b"PROTECTED").expect("write victim");

        let obstruction = dir.path().join("partial");
        std::os::unix::fs::symlink(&outside, &obstruction).expect("plant symlink");

        clear_partial_dir_obstruction(&obstruction).expect("clear the obstruction");

        assert!(
            fs::symlink_metadata(&obstruction).is_err(),
            "the symlink standing at the partial-dir name must be removed",
        );
        assert_eq!(
            fs::read(&victim).expect("victim still readable"),
            b"PROTECTED",
            "the symlink's target must be untouched - the link is unlinked, never followed",
        );
        assert!(
            outside.is_dir(),
            "the directory the link pointed at survives"
        );
    }

    /// The companion that keeps the test above honest: an ordinary regular file
    /// at the name is cleared too (upstream's predicate is `!S_ISDIR`, not
    /// "is a symlink"), and an existing DIRECTORY is reused rather than removed.
    #[cfg(unix)]
    #[test]
    fn clearing_the_partial_dir_removes_a_regular_file_and_reuses_a_directory() {
        let dir = tempdir().expect("tempdir");

        let regular = dir.path().join("regular");
        fs::write(&regular, b"obstruction").expect("write obstruction");
        clear_partial_dir_obstruction(&regular).expect("clear the regular file");
        assert!(
            fs::symlink_metadata(&regular).is_err(),
            "a regular file at the partial-dir name is cleared",
        );

        let existing = dir.path().join("existing");
        fs::create_dir(&existing).expect("mkdir existing");
        let inhabitant = existing.join("staged");
        fs::write(&inhabitant, b"staged").expect("write inhabitant");
        clear_partial_dir_obstruction(&existing).expect("reuse the directory");
        assert!(
            inhabitant.is_file(),
            "an existing partial dir is REUSED, so its contents survive",
        );
    }
}
