use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::LocalCopyError;
use crate::local_copy::create_symlink;

pub(crate) fn compute_backup_path(
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
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(destination))
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
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| OsString::from("backup"));
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
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };

    base.push(backup_name);
    base
}

pub(crate) fn copy_entry_to_backup(
    source: &Path,
    backup_path: &Path,
    file_type: fs::FileType,
) -> Result<(), LocalCopyError> {
    if file_type.is_file() {
        fs::copy(source, backup_path).map_err(|error| {
            LocalCopyError::io("create backup", backup_path.to_path_buf(), error)
        })?;
    } else if file_type.is_symlink() {
        let target = fs::read_link(source).map_err(|error| {
            LocalCopyError::io("read symbolic link", source.to_path_buf(), error)
        })?;
        create_symlink(&target, source, backup_path).map_err(|error| {
            LocalCopyError::io("create symbolic link", backup_path.to_path_buf(), error)
        })?;
    }
    Ok(())
}
