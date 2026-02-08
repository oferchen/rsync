//! Parallel directory entry planning using rayon.
//!
//! This module provides parallel metadata prefetching for directory entries,
//! significantly improving performance when processing directories with many
//! symlinks or when `--one-file-system` is enabled.
//!
//! # Design
//!
//! The planning process is split into two phases:
//!
//! 1. **Parallel prefetch**: Expensive filesystem operations (symlink metadata,
//!    link targets, device IDs) are gathered concurrently using rayon.
//!
//! 2. **Sequential planning**: The actual planning logic runs sequentially
//!    with the prefetched data, maintaining correct ordering and context state.
//!
//! This approach parallelizes I/O-bound syscalls while preserving the
//! deterministic ordering required by rsync's file list protocol.

use std::fs;
use std::io;
#[cfg(unix)]
use std::path::Path;

use rayon::prelude::*;

use super::support::DirectoryEntry;

/// Prefetched metadata for a directory entry.
///
/// Contains optional data that may be needed during planning, gathered
/// in parallel to avoid sequential syscall bottlenecks.
#[derive(Debug)]
pub(crate) struct PrefetchedEntryData {
    /// Index of the entry in the original slice (for ordering).
    #[allow(dead_code)] // Stored for potential reordering verification
    pub(crate) index: usize,
    /// Symlink target metadata, if the entry is a symlink and we need to follow it.
    pub(crate) symlink_target_metadata: Option<io::Result<fs::Metadata>>,
    /// Symlink target path, if needed for safe_links checking.
    pub(crate) symlink_target: Option<io::Result<std::path::PathBuf>>,
    /// Device identifier for one-file-system checks.
    #[cfg(unix)]
    pub(crate) device_id: Option<u64>,
}

/// Configuration for what metadata to prefetch.
#[derive(Clone, Copy)]
pub(crate) struct PrefetchConfig {
    /// Whether to follow symlinks (--copy-links or --copy-dirlinks).
    pub(crate) follow_symlinks: bool,
    /// Whether to read symlink targets (--safe-links).
    pub(crate) read_symlink_targets: bool,
    /// Whether to get device IDs (--one-file-system).
    pub(crate) check_devices: bool,
}

impl PrefetchConfig {
    /// Returns true if any prefetching is needed.
    pub(crate) const fn needs_prefetch(&self) -> bool {
        self.follow_symlinks || self.read_symlink_targets || self.check_devices
    }
}

/// Prefetches metadata for directory entries in parallel.
///
/// Only performs I/O for entries that need it based on the configuration
/// and entry types. Returns results in the same order as the input entries.
///
/// # Arguments
///
/// * `entries` - Directory entries to prefetch metadata for
/// * `config` - Configuration specifying what to prefetch
///
/// # Returns
///
/// Vector of prefetched data, one per entry, in the same order as input.
pub(crate) fn prefetch_entry_metadata(
    entries: &[DirectoryEntry],
    config: PrefetchConfig,
) -> Vec<PrefetchedEntryData> {
    if !config.needs_prefetch() {
        // Fast path: no prefetching needed, return empty prefetch data
        return entries
            .iter()
            .enumerate()
            .map(|(index, _)| PrefetchedEntryData {
                index,
                symlink_target_metadata: None,
                symlink_target: None,
                #[cfg(unix)]
                device_id: None,
            })
            .collect();
    }

    // Parallel prefetch using rayon
    entries
        .par_iter()
        .enumerate()
        .map(|(index, entry)| {
            let entry_type = entry.metadata.file_type();
            let is_symlink = entry_type.is_symlink();
            #[cfg(unix)]
            let is_dir = entry_type.is_dir();

            // Prefetch symlink target metadata if needed
            // Use fs::metadata which follows symlinks (unlike symlink_metadata)
            let symlink_target_metadata = if is_symlink && config.follow_symlinks {
                Some(fs::metadata(&entry.path))
            } else {
                None
            };

            // Prefetch symlink target path if needed for safe_links
            let symlink_target = if is_symlink && config.read_symlink_targets {
                Some(fs::read_link(&entry.path))
            } else {
                None
            };

            // Prefetch device ID if needed for one-file-system
            #[cfg(unix)]
            let device_id = if (is_dir || is_symlink) && config.check_devices {
                get_device_id(&entry.path, &entry.metadata)
            } else {
                None
            };

            PrefetchedEntryData {
                index,
                symlink_target_metadata,
                symlink_target,
                #[cfg(unix)]
                device_id,
            }
        })
        .collect()
}

/// Gets the device ID for a path.
#[cfg(unix)]
fn get_device_id(path: &Path, metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    // For symlinks, we need to get the target's device
    if metadata.file_type().is_symlink() {
        fs::metadata(path).ok().map(|m| m.dev())
    } else {
        Some(metadata.dev())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    fn create_test_entry(path: std::path::PathBuf) -> DirectoryEntry {
        let metadata = fs::symlink_metadata(&path).unwrap();
        let file_name = path.file_name().unwrap().to_os_string();
        DirectoryEntry {
            path,
            file_name,
            metadata,
        }
    }

    #[test]
    fn prefetch_returns_correct_count() {
        let dir = tempdir().unwrap();
        let file1 = dir.path().join("a.txt");
        let file2 = dir.path().join("b.txt");
        File::create(&file1).unwrap();
        File::create(&file2).unwrap();

        let entries = vec![create_test_entry(file1), create_test_entry(file2)];

        let config = PrefetchConfig {
            follow_symlinks: false,
            read_symlink_targets: false,
            check_devices: false,
        };

        let prefetched = prefetch_entry_metadata(&entries, config);
        assert_eq!(prefetched.len(), 2);
        assert_eq!(prefetched[0].index, 0);
        assert_eq!(prefetched[1].index, 1);
    }

    #[test]
    fn prefetch_skips_when_not_needed() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        File::create(&file).unwrap();

        let entries = vec![create_test_entry(file)];

        let config = PrefetchConfig {
            follow_symlinks: false,
            read_symlink_targets: false,
            check_devices: false,
        };

        let prefetched = prefetch_entry_metadata(&entries, config);
        assert!(prefetched[0].symlink_target_metadata.is_none());
        assert!(prefetched[0].symlink_target.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn prefetch_gets_device_id_for_directories() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let entries = vec![create_test_entry(subdir)];

        let config = PrefetchConfig {
            follow_symlinks: false,
            read_symlink_targets: false,
            check_devices: true,
        };

        let prefetched = prefetch_entry_metadata(&entries, config);
        assert!(prefetched[0].device_id.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn prefetch_follows_symlinks_when_configured() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        File::create(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let entries = vec![create_test_entry(link)];

        let config = PrefetchConfig {
            follow_symlinks: true,
            read_symlink_targets: true,
            check_devices: false,
        };

        let prefetched = prefetch_entry_metadata(&entries, config);
        assert!(prefetched[0].symlink_target_metadata.is_some());
        assert!(
            prefetched[0]
                .symlink_target_metadata
                .as_ref()
                .unwrap()
                .is_ok()
        );
        assert!(prefetched[0].symlink_target.is_some());
        assert!(prefetched[0].symlink_target.as_ref().unwrap().is_ok());
    }

    #[test]
    fn prefetch_config_needs_prefetch() {
        let config_none = PrefetchConfig {
            follow_symlinks: false,
            read_symlink_targets: false,
            check_devices: false,
        };
        assert!(!config_none.needs_prefetch());

        let config_symlinks = PrefetchConfig {
            follow_symlinks: true,
            read_symlink_targets: false,
            check_devices: false,
        };
        assert!(config_symlinks.needs_prefetch());

        let config_devices = PrefetchConfig {
            follow_symlinks: false,
            read_symlink_targets: false,
            check_devices: true,
        };
        assert!(config_devices.needs_prefetch());
    }
}
