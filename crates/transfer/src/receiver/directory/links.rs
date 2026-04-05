//! Symlink and hardlink creation from the received file list.
//!
//! Handles symbolic link creation (Unix-only with `--links`) and hardlink
//! creation (leader/follower via `hardlink_idx`). Protocol 28-29 entries are
//! normalized to use `hardlink_idx` and `hlink_first` during file list
//! reception, so this module handles both protocol versions uniformly.

use std::fs;
use std::path::Path;

use logging::debug_log;
#[cfg(unix)]
use logging::info_log;

use crate::generator::ItemFlags;
use crate::receiver::ReceiverContext;

impl ReceiverContext {
    /// Creates symbolic links from the file list entries.
    ///
    /// Iterates through the received file list, finds symlink entries with
    /// `preserve_links` enabled, and creates them on the destination filesystem.
    /// Existing symlinks pointing to the correct target are skipped (quick-check).
    /// Existing files/symlinks at the destination path are removed before creation.
    ///
    /// Emits MSG_INFO itemize frames for each symlink created or found up-to-date
    /// when the daemon has itemize output enabled.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1544` - `if (preserve_links && ftype == FT_SYMLINK)`
    /// - `generator.c:1591` - `atomic_create(file, fname, sl, ...)`
    #[cfg(unix)]
    pub(in crate::receiver) fn create_symlinks<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        writer: &mut W,
    ) {
        if !self.config.flags.links || self.config.flags.dry_run {
            return;
        }

        for entry in &self.file_list {
            if !entry.is_symlink() {
                continue;
            }

            let target = match entry.link_target() {
                Some(t) => t,
                None => continue,
            };

            let relative_path = entry.path();

            // upstream: generator.c:1547 - skip unsafe symlinks when --safe-links
            // is set. Check stays here (not in sanitize_file_list) to preserve
            // protocol index alignment with the sender.
            if self.config.flags.safe_links
                && crate::symlink_safety::is_unsafe_symlink(target.as_os_str(), relative_path)
            {
                // upstream: generator.c:1554 - log skipped unsafe symlinks
                info_log!(
                    Name,
                    1,
                    "skipping unsafe symlink \"{}\" -> \"{}\"",
                    relative_path.display(),
                    target.display()
                );
                continue;
            }

            let link_path = dest_dir.join(relative_path);

            // upstream: generator.c:1561 - quick_check_ok(FT_SYMLINK, ...)
            if let Ok(existing_target) = std::fs::read_link(&link_path) {
                if existing_target == *target {
                    // upstream: generator.c:1565 - symlink up-to-date, metadata only
                    let iflags = ItemFlags::from_raw(0);
                    let _ = self.emit_itemize(writer, &iflags, entry);
                    continue;
                }
                let _ = std::fs::remove_file(&link_path);
            } else if link_path.symlink_metadata().is_ok() {
                let _ = std::fs::remove_file(&link_path);
            }

            // Ensure parent directory exists for --relative paths.
            // upstream: generator.c:1317-1326 - make_path() for relative_paths
            if let Some(parent) = link_path.parent() {
                let _ = fs::create_dir_all(parent);
            }

            // upstream: generator.c:1591 - atomic_create() -> do_symlink()
            if let Err(e) = std::os::unix::fs::symlink(target, &link_path) {
                debug_log!(
                    Recv,
                    1,
                    "failed to create symlink {} -> {}: {}",
                    link_path.display(),
                    target.display(),
                    e
                );
            } else {
                // upstream: generator.c:1594 - itemize new symlink after creation
                let iflags =
                    ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
                let _ = self.emit_itemize(writer, &iflags, entry);
            }
        }
    }

    /// No-op on non-Unix platforms where symlinks are not supported.
    #[cfg(not(unix))]
    pub(in crate::receiver) fn create_symlinks<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        _dest_dir: &Path,
        _writer: &mut W,
    ) {
    }

    /// Creates hard links for hardlink follower entries after the leader has been transferred.
    ///
    /// Hardlinked files are grouped by a shared `hardlink_idx`. The first file in
    /// each group (the "leader", with `hlink_first = true`) is transferred normally.
    /// Subsequent files ("followers") are created as hard links to the leader's
    /// destination path instead of being transferred independently.
    ///
    /// This method works uniformly for both protocol versions:
    /// - Protocol 30+: `hardlink_idx` and `hlink_first` come from the wire encoding
    /// - Protocol 28-29: `normalize_pre30_hardlinks()` assigns synthetic
    ///   `hardlink_idx` and `hlink_first` from (dev, ino) pairs during file list
    ///   reception, so both versions use the same leader/follower logic here.
    ///
    /// Uses `HardlinkApplyTracker` for leader path tracking and deferred follower
    /// resolution. Leaders committed during pipelined transfer are already recorded
    /// in the tracker; this method records any remaining leaders (e.g., files that
    /// matched quick-check and were not transferred) and links all followers.
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:hard_link_check()` - skips followers, links to leader
    /// - `hlink.c:finish_hard_link()` - creates links after leader transfer completes
    /// - `generator.c:1539` - `F_HLINK_NOT_FIRST` check before `hard_link_check()`
    pub(in crate::receiver) fn create_hardlinks<W: crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        dest_dir: &Path,
        writer: &mut W,
    ) {
        if !self.config.flags.hard_links || self.config.flags.dry_run {
            return;
        }

        // Protocol 30+: use HardlinkApplyTracker for leader/follower resolution.
        // Take the tracker temporarily to avoid borrow conflicts with self.emit_itemize().
        if let Some(mut tracker) = self.hardlink_tracker.take() {
            // First pass: ensure all leaders are recorded in the tracker.
            // Leaders committed during pipelined transfer are already recorded;
            // this covers leaders that matched quick-check (not transferred) or
            // were processed via the sync path.
            for entry in &self.file_list {
                if entry.flags().hlink_first() {
                    let gnum = match entry.hardlink_idx() {
                        Some(idx) => idx,
                        None => continue,
                    };
                    // Skip if already recorded during pipelined commit.
                    if tracker.leader_path(gnum).is_some() {
                        continue;
                    }
                    let relative_path = entry.path();
                    let dest_path = if relative_path.as_os_str() == "." {
                        dest_dir.to_path_buf()
                    } else {
                        dest_dir.join(relative_path)
                    };
                    // No deferred followers expected here since we process
                    // followers in the second pass below.
                    let _ = tracker.record_leader(gnum, dest_path);
                }
            }

            // Second pass: link followers to their leaders.
            for entry in &self.file_list {
                if !entry.flags().hlinked() || entry.flags().hlink_first() {
                    continue;
                }
                let leader_idx = match entry.hardlink_idx() {
                    Some(idx) => idx,
                    None => continue,
                };

                let leader_path = match tracker.leader_path(leader_idx) {
                    Some(p) => p.to_path_buf(),
                    None => {
                        debug_log!(
                            Recv,
                            1,
                            "hardlink follower {} references unknown leader index {}",
                            entry.name(),
                            leader_idx
                        );
                        continue;
                    }
                };

                let relative_path = entry.path();
                let link_path = dest_dir.join(relative_path);

                // Quick-check: if destination already hard-links to the leader, skip.
                if let Ok(link_meta) = fs::symlink_metadata(&link_path) {
                    if let Ok(leader_meta) = fs::symlink_metadata(&leader_path) {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            if link_meta.dev() == leader_meta.dev()
                                && link_meta.ino() == leader_meta.ino()
                            {
                                // upstream: hlink.c - hardlink already correct, metadata only
                                let iflags = ItemFlags::from_raw(0);
                                let _ = self.emit_itemize(writer, &iflags, entry);
                                continue;
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = (link_meta, leader_meta);
                        }
                    }
                    // Remove existing file so we can create the hard link.
                    let _ = fs::remove_file(&link_path);
                }

                // Ensure parent directory exists.
                if let Some(parent) = link_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                // upstream: hlink.c:maybe_hard_link() -> atomic_create() -> do_link()
                if let Err(e) = fs::hard_link(&leader_path, &link_path) {
                    debug_log!(
                        Recv,
                        1,
                        "failed to hard link {} => {}: {}",
                        link_path.display(),
                        leader_path.display(),
                        e
                    );
                } else {
                    // upstream: hlink.c:finish_hard_link() - itemize new hardlink
                    let iflags = ItemFlags::from_raw(
                        ItemFlags::ITEM_LOCAL_CHANGE
                            | ItemFlags::ITEM_XNAME_FOLLOWS
                            | ItemFlags::ITEM_IS_NEW,
                    );
                    let _ = self.emit_itemize(writer, &iflags, entry);
                }
            }

            // Resolve any remaining deferred followers from incremental commits.
            // upstream: hlink.c:finish_hard_link() - final pass
            let (linked, errors) = tracker.resolve_deferred();
            if linked > 0 {
                debug_log!(Recv, 2, "resolved {} deferred hardlink followers", linked);
            }
            for (path, err) in errors {
                debug_log!(
                    Recv,
                    1,
                    "failed to resolve deferred hardlink {}: {}",
                    path.display(),
                    err
                );
            }

            self.hardlink_tracker = Some(tracker);
        }

        // Protocol 28-29 hardlinks are normalized to hardlink_idx/hlink_first
        // during file list reception (normalize_pre30_hardlinks), so the
        // protocol 30+ path above handles both versions uniformly.
    }
}
