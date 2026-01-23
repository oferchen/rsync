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

pub(crate) fn copy_entry_to_backup(
    source: &Path,
    backup_path: &Path,
    file_type: fs::FileType,
) -> Result<(), LocalCopyError> {
    if file_type.is_file() {
        fs::copy(source, backup_path)
            .map_err(|error| LocalCopyError::io("create backup", backup_path, error))?;
    } else if file_type.is_symlink() {
        let target = fs::read_link(source)
            .map_err(|error| LocalCopyError::io("read symbolic link", source, error))?;
        create_symlink(&target, source, backup_path)
            .map_err(|error| LocalCopyError::io("create symbolic link", backup_path, error))?;
    }
    Ok(())
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
