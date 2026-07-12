//! Backup file creation for `--backup` and `--backup-dir`.
//!
//! Computes backup paths (with optional suffix and directory prefix) and
//! creates the backup copy or symlink before the destination is overwritten.
//!
//! upstream: backup.c:make_backup() - backup path computation and creation

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::LocalCopyError;
use crate::local_copy::context::BackupStrategy;
use crate::local_copy::create_symlink;
#[cfg(unix)]
use crate::local_copy::map_metadata_error;

/// Computes the backup file path for a destination file.
///
/// When `backup_dir` is `Some`, the backup is placed under that directory
/// preserving the relative path structure. Otherwise, the backup is placed
/// alongside the destination with the given `suffix` appended.
///
/// # Upstream Reference
///
/// - `backup.c:get_backup_name()` - path computation for backup files
#[must_use]
pub fn compute_backup_path(
    destination_root: &Path,
    destination: &Path,
    relative: Option<&Path>,
    backup_dir: Option<&Path>,
    suffix: &OsStr,
) -> PathBuf {
    let relative_path = if let Some(rel) = relative {
        rel.to_path_buf()
    } else if let Ok(stripped) = destination.strip_prefix(destination_root) {
        if stripped.as_os_str().is_empty() {
            destination
                .file_name()
                .map_or_else(|| PathBuf::from(destination), PathBuf::from)
        } else {
            stripped.to_path_buf()
        }
    } else if let Some(name) = destination.file_name() {
        PathBuf::from(name)
    } else {
        PathBuf::from(destination)
    };

    let mut backup_name = relative_path
        .file_name()
        .map_or_else(|| OsString::from("backup"), |name| name.to_os_string());
    if !suffix.is_empty() {
        backup_name.push(suffix);
    }

    let mut base = if let Some(dir) = backup_dir {
        let mut base = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            destination_root.join(dir)
        };
        if let Some(parent) = relative_path.parent()
            && !parent.as_os_str().is_empty()
        {
            base = base.join(parent);
        }
        base
    } else {
        destination
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    };

    base.push(backup_name);
    base
}

/// Copies a regular file, recreates a symlink, or re-materialises a
/// device/FIFO/socket node at the backup path.
///
/// Returns the [`BackupStrategy`] that placed the backup, or `None` when the
/// entry is a non-regular file that upstream declines to back up (mirrors
/// `backup.c:306-317`, where `make_backup` returns 3 and leaves no backup:
/// a device without `am_root && --devices`, or a special without
/// `--specials`).
// upstream: backup.c:make_backup() - copy-tree fallback (COPY / SYMLINK /
// DEVICE branches). Device and special nodes are recreated via do_mknod_at
// (backup.c:278-285), gated on am_root+preserve_devices / preserve_specials.
pub(crate) fn copy_entry_to_backup(
    source: &Path,
    backup_path: &Path,
    file_type: fs::FileType,
    devices_enabled: bool,
    specials_enabled: bool,
    fake_super: bool,
) -> Result<Option<BackupStrategy>, LocalCopyError> {
    if file_type.is_file() {
        fs::copy(source, backup_path)
            .map_err(|error| LocalCopyError::io("create backup", backup_path, error))?;
        return Ok(Some(BackupStrategy::Copy));
    }
    if file_type.is_symlink() {
        let target = fs::read_link(source)
            .map_err(|error| LocalCopyError::io("read symbolic link", source, error))?;
        create_symlink(&target, source, backup_path)
            .map_err(|error| LocalCopyError::io("create symbolic link", backup_path, error))?;
        return Ok(Some(BackupStrategy::Symlink));
    }
    #[cfg(unix)]
    {
        copy_special_to_backup(
            source,
            backup_path,
            devices_enabled,
            specials_enabled,
            fake_super,
        )
    }
    #[cfg(not(unix))]
    {
        // Native Windows has no device/FIFO/socket nodes to back up; upstream's
        // do_mknod path is Unix-only, so there is nothing to recreate here.
        let _ = (
            source,
            backup_path,
            devices_enabled,
            specials_enabled,
            fake_super,
        );
        Ok(None)
    }
}

/// Re-materialises a device, FIFO, or socket node at `backup_path` from the
/// existing destination node at `source`, mirroring upstream
/// `backup.c:278-285`.
///
/// Returns `Some(BackupStrategy::Device)` once the node is recreated (upstream
/// emits `make_backup: DEVICE` for both devices and specials), or `None` when
/// the preserve gates decline it (upstream `make_backup` returns 3 without
/// placing a backup). Under `--fake-super` the node is virtualised as a `0600`
/// placeholder carrying the `%stat` xattr, matching `syscall.c:do_mknod()`'s
/// `am_root < 0` branch.
// upstream: backup.c:278 - `(am_root && preserve_devices && IS_DEVICE(mode))
// || (preserve_specials && IS_SPECIAL(mode))` gates `do_mknod_at`. am_root is
// non-zero for real root, --super, and --fake-super (options.c:90).
#[cfg(unix)]
fn copy_special_to_backup(
    source: &Path,
    backup_path: &Path,
    devices_enabled: bool,
    specials_enabled: bool,
    fake_super: bool,
) -> Result<Option<BackupStrategy>, LocalCopyError> {
    use std::os::unix::fs::FileTypeExt;

    let source_meta = fs::symlink_metadata(source)
        .map_err(|error| LocalCopyError::io("stat backup source", source, error))?;
    let source_type = source_meta.file_type();
    let is_device = source_type.is_char_device() || source_type.is_block_device();

    let should_backup = if is_device {
        (::metadata::am_root() || fake_super) && devices_enabled
    } else if source_type.is_fifo() || source_type.is_socket() {
        specials_enabled
    } else {
        false
    };
    if !should_backup {
        return Ok(None);
    }

    if is_device {
        ::metadata::create_device_node_with_fake_super(backup_path, &source_meta, fake_super)
            .map_err(map_metadata_error)?;
    } else {
        ::metadata::create_fifo_with_fake_super(backup_path, &source_meta, fake_super)
            .map_err(map_metadata_error)?;
    }
    Ok(Some(BackupStrategy::Device))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::Path;

    #[test]
    fn compute_backup_path_with_suffix_only() {
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/dest/file.txt"),
            None,
            None,
            OsStr::new("~"),
        );
        assert_eq!(result, PathBuf::from("/dest/file.txt~"));
    }

    #[test]
    fn compute_backup_path_with_empty_suffix() {
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/dest/file.txt"),
            None,
            None,
            OsStr::new(""),
        );
        assert_eq!(result, PathBuf::from("/dest/file.txt"));
    }

    #[test]
    fn compute_backup_path_with_relative_path() {
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/dest/subdir/file.txt"),
            Some(Path::new("subdir/file.txt")),
            None,
            OsStr::new(".bak"),
        );
        assert_eq!(result, PathBuf::from("/dest/subdir/file.txt.bak"));
    }

    #[test]
    fn compute_backup_path_with_absolute_backup_dir() {
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/dest/file.txt"),
            None,
            Some(Path::new("/backup")),
            OsStr::new("~"),
        );
        assert_eq!(result, PathBuf::from("/backup/file.txt~"));
    }

    #[test]
    fn compute_backup_path_with_relative_backup_dir() {
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/dest/file.txt"),
            None,
            Some(Path::new(".backups")),
            OsStr::new("~"),
        );
        assert_eq!(result, PathBuf::from("/dest/.backups/file.txt~"));
    }

    #[test]
    fn compute_backup_path_preserves_directory_structure_in_backup_dir() {
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/dest/subdir/deep/file.txt"),
            Some(Path::new("subdir/deep/file.txt")),
            Some(Path::new("/backup")),
            OsStr::new("~"),
        );
        assert_eq!(result, PathBuf::from("/backup/subdir/deep/file.txt~"));
    }

    #[test]
    fn compute_backup_path_destination_is_root() {
        // When destination matches destination_root exactly
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/dest"),
            None,
            None,
            OsStr::new("~"),
        );
        assert_eq!(result, PathBuf::from("/dest~"));
    }

    #[test]
    fn compute_backup_path_destination_not_under_root() {
        // When destination is not under destination_root
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/other/file.txt"),
            None,
            None,
            OsStr::new("~"),
        );
        assert_eq!(result, PathBuf::from("/other/file.txt~"));
    }

    #[test]
    fn compute_backup_path_no_file_name() {
        // When destination has no file name (e.g., root path)
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/"),
            None,
            None,
            OsStr::new("~"),
        );
        // Should use "backup" as default name
        assert!(result.to_string_lossy().contains("backup"));
    }

    #[test]
    fn compute_backup_path_nested_with_backup_dir_and_relative() {
        let result = compute_backup_path(
            Path::new("/dest"),
            Path::new("/dest/a/b/c.txt"),
            Some(Path::new("a/b/c.txt")),
            Some(Path::new("/backups")),
            OsStr::new(".old"),
        );
        assert_eq!(result, PathBuf::from("/backups/a/b/c.txt.old"));
    }

    #[test]
    fn compute_backup_path_relative_backup_dir_with_subdirectory() {
        let result = compute_backup_path(
            Path::new("/project"),
            Path::new("/project/src/main.rs"),
            Some(Path::new("src/main.rs")),
            Some(Path::new("backup")),
            OsStr::new("~"),
        );
        assert_eq!(result, PathBuf::from("/project/backup/src/main.rs~"));
    }
}
