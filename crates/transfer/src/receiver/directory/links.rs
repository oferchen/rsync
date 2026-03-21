//! Symlink and hardlink creation from the received file list.
//!
//! Handles symbolic link creation (Unix-only with `--links`), protocol 30+
//! hardlink creation (leader/follower via `hardlink_idx`), and pre-30 hardlink
//! creation via `(dev, ino)` pairs.

use std::fs;
use std::path::Path;

use logging::{debug_log, info_log};

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
    /// In the rsync protocol (30+), hardlinked files are grouped by a shared index.
    /// The first file in each group (the "leader", `hardlink_idx == u32::MAX`) is
    /// transferred normally. Subsequent files ("followers") carry the leader's file
    /// list index in their `hardlink_idx` field and are created as hard links to the
    /// leader's destination path instead of being transferred independently.
    ///
    /// For protocol 28-29, hardlinks are identified by (dev, ino) pairs. This method
    /// handles both cases by building a mapping from hardlink identifiers to leader
    /// destination paths.
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:hard_link_check()` - skips followers, links to leader
    /// - `hlink.c:finish_hard_link()` - creates links after leader transfer completes
    /// - `generator.c:1539` - `F_HLINK_NOT_FIRST` check before `hard_link_check()`
    pub(in crate::receiver) fn create_hardlinks<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        writer: &mut W,
    ) {
        if !self.config.flags.hard_links || self.config.flags.dry_run {
            return;
        }

        // Protocol 30+: use hardlink_idx to map followers to leaders.
        // Build a map from file list index -> destination path for leaders.
        let mut leader_paths: std::collections::HashMap<u32, std::path::PathBuf> =
            std::collections::HashMap::new();

        // First pass: record leader paths keyed by their readdir-order wire NDX
        // (stored in hardlink_idx during receive_file_list before sorting).
        // upstream: flist.c - F_HL_GNUM stores the readdir-order wire NDX of the leader
        for entry in &self.file_list {
            if entry.flags().hlink_first() {
                let leader_gnum = match entry.hardlink_idx() {
                    Some(idx) => idx,
                    None => continue,
                };
                let relative_path = entry.path();
                let dest_path = if relative_path.as_os_str() == "." {
                    dest_dir.to_path_buf()
                } else {
                    dest_dir.join(relative_path)
                };
                leader_paths.insert(leader_gnum, dest_path);
            }
        }

        // Second pass: create hard links for followers (hlinked but NOT hlink_first).
        for entry in &self.file_list {
            if !entry.flags().hlinked() || entry.flags().hlink_first() {
                continue;
            }
            let leader_idx = match entry.hardlink_idx() {
                Some(idx) => idx,
                None => continue,
            };

            let leader_path = match leader_paths.get(&leader_idx) {
                Some(p) => p,
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
                if let Ok(leader_meta) = fs::symlink_metadata(leader_path) {
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
            if let Err(e) = fs::hard_link(leader_path, &link_path) {
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

        // Protocol 28-29: use (dev, ino) pairs to identify hardlink groups.
        // Build groups from entries that have hardlink_dev/ino set but no hardlink_idx.
        if self.protocol.as_u8() < 30 {
            self.create_hardlinks_pre30(dest_dir, writer);
        }
    }

    /// Creates hard links for protocol 28-29 using (dev, ino) pairs.
    ///
    /// In protocols before 30, hardlinks are identified by matching (dev, ino)
    /// pairs across file list entries. The first entry with a given (dev, ino) is
    /// the leader; subsequent entries are followers that should be hard-linked.
    fn create_hardlinks_pre30<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        writer: &mut W,
    ) {
        use std::collections::HashMap;
        use std::path::PathBuf;

        // Map from (dev, ino) -> first file's destination path.
        let mut dev_ino_map: HashMap<(i64, i64), PathBuf> = HashMap::new();

        for entry in &self.file_list {
            if !entry.is_file() {
                continue;
            }

            let dev = match entry.hardlink_dev() {
                Some(d) => d,
                None => continue,
            };
            let ino = match entry.hardlink_ino() {
                Some(i) => i,
                None => continue,
            };

            let relative_path = entry.path();
            let dest_path = dest_dir.join(relative_path);

            match dev_ino_map.entry((dev, ino)) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(dest_path);
                }
                std::collections::hash_map::Entry::Occupied(e) => {
                    let leader_path = e.get();

                    // Quick-check: skip if already linked.
                    if let Ok(link_meta) = fs::symlink_metadata(&dest_path) {
                        if let Ok(leader_meta) = fs::symlink_metadata(leader_path) {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::MetadataExt;
                                if link_meta.dev() == leader_meta.dev()
                                    && link_meta.ino() == leader_meta.ino()
                                {
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
                        let _ = fs::remove_file(&dest_path);
                    }

                    if let Some(parent) = dest_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }

                    if let Err(e) = fs::hard_link(leader_path, &dest_path) {
                        debug_log!(
                            Recv,
                            1,
                            "failed to hard link {} => {}: {}",
                            dest_path.display(),
                            leader_path.display(),
                            e
                        );
                    } else {
                        let iflags = ItemFlags::from_raw(
                            ItemFlags::ITEM_LOCAL_CHANGE
                                | ItemFlags::ITEM_XNAME_FOLLOWS
                                | ItemFlags::ITEM_IS_NEW,
                        );
                        let _ = self.emit_itemize(writer, &iflags, entry);
                    }
                }
            }
        }
    }
}
