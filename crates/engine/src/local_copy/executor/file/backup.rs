//! Backup file creation for `--backup` and `--backup-dir`.
//!
//! Computes backup paths (with optional suffix and directory prefix) and
//! creates the backup copy or symlink before the destination is overwritten.
//!
//! upstream: backup.c:make_backup() - backup path computation and creation

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::local_copy::LocalCopyError;
use crate::local_copy::context::BackupStrategy;
use crate::local_copy::create_symlink;
#[cfg(unix)]
use crate::local_copy::map_metadata_error;

/// Duplicates a destination's pre-transfer bytes into the operator-named
/// backup path, resolving that path with the ownership walk.
///
/// Used by the `--inplace --backup` paths, where the destination inode is
/// rewritten in place rather than replaced, so the pre-image must be COPIED
/// aside instead of renamed.
///
/// upstream: generator.c:2279-2300 and generator.c:2327-2350 - the in-place
/// backup BYPASSES `make_backup()`, so the generator raises
/// `operator_path_resolve` around `get_backup_name()` and the `copy_file()` /
/// `do_open_at()` that follow, clearing it again on every exit path. Without
/// it the open of `backupptr` resolves with libc and FOLLOWS a symlink planted
/// at the `--backup-dir` leaf. `std::fs::copy` has exactly that behaviour,
/// which is what upstream's `operator-path-inplace-backup-dir` cell observes as
/// an escape out of the transfer tree.
///
/// Permissions are taken from the source and applied through the open
/// descriptor, matching `fs::copy` (and upstream's
/// `copy_file(..., back_file->mode)`) without ever re-resolving the path the
/// walk has already vetted.
///
/// A vanished source surfaces as [`io::ErrorKind::NotFound`], exactly as
/// `fs::copy` did, so callers keep their existing "nothing to back up" arm.
pub fn copy_pre_image_to_backup(source: &Path, backup_path: &Path) -> io::Result<()> {
    let mut reader = fs::File::open(source)?;
    let permissions = reader.metadata()?.permissions();

    #[cfg(unix)]
    let mut writer = {
        use std::os::unix::fs::PermissionsExt as _;

        // Mask to the permission bits: `mode()` carries the file-type bits too,
        // and only the low 12 are meaningful as an `O_CREAT` mode.
        fast_io::operator_open_write_create(backup_path, permissions.mode() & 0o7777)?
    };
    // Non-Unix has no ownership walk to run; degrade to a plain create, as the
    // other operator-path helpers do.
    #[cfg(not(unix))]
    let mut writer = fs::File::create(backup_path)?;

    io::copy(&mut reader, &mut writer)?;
    writer.set_permissions(permissions)?;
    Ok(())
}

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

/// Creates the intermediate directories leading to a backup path, mirroring
/// upstream's `copy_valid_path` (backup.c:61-154).
///
/// For each path element that does not yet exist, a directory is created and -
/// when `--backup-dir` is in use - the corresponding destination directory's
/// mode/owner/mtime is copied onto it, exactly as upstream does after each
/// `do_mkdir_at` via `x_stat` + `make_file` + `set_file_attrs`
/// (backup.c:101-142). Pre-existing directories are left untouched, matching
/// upstream's `EEXIST`/`validate_backup_dir` skip (backup.c:102-105).
///
/// A path element that exists but is not a directory is cleared before the
/// directory is created, mirroring `validate_backup_dir` (backup.c:48-53),
/// where a non-directory triggers `delete_item(...DEL_FOR_BACKUP|DEL_RECURSE)`
/// so the element can be recreated as a directory.
///
/// When `--backup-dir` is not set the backup lands alongside the destination
/// and every element already exists, so no directory is created and no
/// attribute copy is performed (upstream only runs `copy_valid_path` when
/// `backup_dir` is set - backup.c:159).
pub(crate) fn create_backup_parents(
    destination_root: &Path,
    backup_dir: Option<&Path>,
    parent: &Path,
    metadata_options: &::metadata::MetadataOptions,
) -> Result<(), LocalCopyError> {
    if parent.as_os_str().is_empty() {
        return Ok(());
    }

    // Without --backup-dir the backup lands alongside the destination and its
    // parent already exists; upstream runs no copy_valid_path here (backup.c:159
    // vs :179), so a plain create_dir_all suffices with no attribute copy.
    let Some(backup_dir) = backup_dir else {
        return fs::create_dir_all(parent)
            .map_err(|error| LocalCopyError::io("create backup directory", parent, error));
    };

    // Delegate the `--backup-dir` subtree to the shared helper, using a plain
    // recursive create for each element (the local path has no receiver
    // sandbox to anchor against).
    create_backup_dir_parents(
        destination_root,
        backup_dir,
        parent,
        metadata_options,
        |path| fs::create_dir_all(path),
    )
    .map_err(|error| LocalCopyError::io("create backup directory", parent, error))
}

/// Creates the `--backup-dir` parent tree for a backup path, inheriting each
/// freshly-created subdirectory's attributes from the corresponding
/// destination directory and clearing any non-directory obstruction.
///
/// Mirrors upstream `copy_valid_path` (backup.c:61-154) plus
/// `validate_backup_dir` (backup.c:48-53): the backup root is ensured first
/// (backup.c:165 `make_path`), then only the relative portion *below* the root
/// is validated element-by-element (backup.c:67 walks
/// `rel = backup_dir_buf + backup_dir_len`, never re-validating the path above
/// `backup_dir`). Pre-existing directories are skipped (backup.c:102-105
/// `EEXIST`/`validate_backup_dir`); a non-directory obstruction (including a
/// symlink) is removed so the element can be recreated as a directory
/// (backup.c:48-53 `delete_item(...DEL_FOR_BACKUP|DEL_RECURSE)`); and each new
/// directory inherits the corresponding destination directory's attributes
/// (backup.c:115-138 `x_stat` + `make_file` + `set_file_attrs`).
///
/// Directory creation is delegated to `mkdir` so the network receiver commit
/// path can anchor the leaf `mkdir` through its destination sandbox dirfd
/// (SEC-1.j) while the local path uses a plain `create_dir_all`. `mkdir` must
/// be idempotent for an already-existing directory, matching upstream's
/// `EEXIST` skip. Shared by the local-copy executor and the network receiver
/// so a `--backup-dir` subtree gets identical attribute inheritance and
/// obstruction handling regardless of transport.
///
/// Walking from `backup_dir` (rather than the filesystem root) avoids
/// misreading symlinked ancestors (e.g. macOS `/var` -> `/private/var`) as
/// obstructions.
pub fn create_backup_dir_parents<F>(
    destination_root: &Path,
    backup_dir: &Path,
    parent: &Path,
    metadata_options: &::metadata::MetadataOptions,
    mut mkdir: F,
) -> io::Result<()>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    let backup_root = if backup_dir.is_absolute() {
        backup_dir.to_path_buf()
    } else {
        destination_root.join(backup_dir)
    };

    // upstream: backup.c:165 make_path(backup_dir_buf, 0) - ensure the backup
    // directory root exists before validating subdirs.
    mkdir(&backup_root)?;

    let Ok(rel) = parent.strip_prefix(&backup_root) else {
        // parent is not under backup_root (unexpected path shape); fall back to
        // a plain create so the backup still lands somewhere valid.
        return mkdir(parent);
    };

    let mut current = backup_root.clone();
    for component in rel.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => continue,
            // upstream: backup.c:48-53 - a non-directory (including a symlink) in
            // the way is removed so it can be recreated as a directory. A
            // non-directory is never a recursive tree, so a plain unlink mirrors
            // `delete_item` for this case (backup.c:50); `remove_file` drops a
            // symlink without following it, matching the `lstat`-based check.
            Ok(_) => fs::remove_file(&current)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        mkdir(&current)?;

        apply_backup_dir_attrs(destination_root, &backup_root, &current, metadata_options);
    }

    Ok(())
}

/// Copies a freshly-created backup subdirectory's attributes from the
/// corresponding destination directory, mirroring backup.c:115-138.
///
/// The backup subdirectory at `<backup_root>/<rel>` inherits from the
/// destination directory at `<destination_root>/<rel>`, matching upstream's
/// `x_stat(rel, ...)` where `rel` is the relative path under the transfer's
/// destination cwd (backup.c:67,117).
///
/// Best-effort: upstream logs and continues when the source stat or attribute
/// application fails (backup.c:117-118, 121-122), so a missing or unreadable
/// destination directory simply leaves the backup subdirectory with default
/// permissions rather than aborting the transfer.
fn apply_backup_dir_attrs(
    destination_root: &Path,
    backup_root: &Path,
    created: &Path,
    metadata_options: &::metadata::MetadataOptions,
) {
    let Ok(rel) = created.strip_prefix(backup_root) else {
        return;
    };
    if rel.as_os_str().is_empty() {
        return;
    }

    let source_dir = destination_root.join(rel);
    if let Ok(meta) = fs::symlink_metadata(&source_dir)
        && meta.is_dir()
    {
        let _ = ::metadata::apply_file_metadata_with_options(created, &meta, metadata_options);
    }
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

    /// `copy_pre_image_to_backup` replaced a `std::fs::copy`, which carries the
    /// permission bits as well as the bytes. Dropping that second half would
    /// silently re-mode every `--inplace` backup.
    ///
    /// The backup is pre-created with a DIFFERENT mode on purpose: that is the
    /// only shape that discriminates. `O_CREAT` applies its mode argument to a
    /// NEW file only, so against a fresh path the open alone already produces
    /// the right bits and the test would pass whether or not the permissions
    /// are carried. The receiver path (`disk_commit`) has no metadata apply
    /// after the copy, so on a re-run over an existing backup this is the only
    /// thing setting its mode.
    #[test]
    fn copy_pre_image_to_backup_carries_content_and_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("dest");
        let backup = dir.path().join("dest~");
        fs::write(&source, b"PRE-IMAGE").expect("write source");
        fs::write(&backup, b"stale").expect("write pre-existing backup");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).expect("chmod source");
            fs::set_permissions(&backup, fs::Permissions::from_mode(0o600)).expect("chmod backup");
        }

        copy_pre_image_to_backup(&source, &backup).expect("copy the pre-image aside");

        assert_eq!(fs::read(&backup).expect("read backup"), b"PRE-IMAGE");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let mode = fs::metadata(&backup)
                .expect("stat backup")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o640, "backup must inherit the source mode");
        }
    }

    /// upstream `robust_unlink`s an existing backup before writing the new one
    /// (`generator.c:1901`); the `O_TRUNC` create has to reach the same end
    /// state, or a shorter pre-image would leave the previous backup's tail
    /// appended to it.
    #[test]
    fn copy_pre_image_to_backup_truncates_a_longer_pre_existing_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("dest");
        let backup = dir.path().join("dest~");
        fs::write(&source, b"short").expect("write source");
        fs::write(&backup, b"a much longer previous backup").expect("write stale backup");

        copy_pre_image_to_backup(&source, &backup).expect("copy the pre-image aside");

        assert_eq!(fs::read(&backup).expect("read backup"), b"short");
    }

    /// Both call sites branch on this errno for their "nothing to back up" arm,
    /// mirroring `backup.c:make_backup` returning 3 for a vanished entry. It is
    /// the one part of `fs::copy`'s contract they actually depend on.
    #[test]
    fn copy_pre_image_to_backup_reports_notfound_for_a_vanished_source() {
        let dir = tempfile::tempdir().expect("tempdir");

        let error = copy_pre_image_to_backup(&dir.path().join("gone"), &dir.path().join("gone~"))
            .expect_err("a missing source cannot be backed up");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

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
