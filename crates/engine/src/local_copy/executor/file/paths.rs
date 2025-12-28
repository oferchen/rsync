use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::local_copy::LocalCopyError;

pub(crate) fn partial_destination_path(destination: &Path) -> PathBuf {
    let file_name = destination
        .file_name().map_or_else(|| "partial".to_owned(), |name| name.to_string_lossy().to_string());
    let partial_name = format!(".rsync-partial-{file_name}");
    destination.with_file_name(partial_name)
}

pub(crate) fn partial_directory_destination_path(
    destination: &Path,
    partial_dir: &Path,
) -> Result<PathBuf, LocalCopyError> {
    let base_dir = if partial_dir.is_absolute() {
        partial_dir.to_path_buf()
    } else {
        let parent = destination
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        parent.join(partial_dir)
    };
    fs::create_dir_all(&base_dir)
        .map_err(|error| LocalCopyError::io("create partial directory", base_dir.clone(), error))?;
    let file_name = destination
        .file_name().map_or_else(|| OsStr::new("partial").to_os_string(), |name| name.to_os_string());
    Ok(base_dir.join(file_name))
}

pub(crate) fn temporary_destination_path(
    destination: &Path,
    unique: usize,
    temp_dir: Option<&Path>,
) -> PathBuf {
    let file_name = destination
        .file_name().map_or_else(|| "temp".to_owned(), |name| name.to_string_lossy().to_string());
    let temp_name = format!(".rsync-tmp-{file_name}-{}-{}", process::id(), unique);
    match temp_dir {
        Some(dir) => dir.join(temp_name),
        None => destination.with_file_name(temp_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_destination_path_adds_prefix() {
        let dest = Path::new("/path/to/file.txt");
        let partial = partial_destination_path(dest);
        assert!(partial.to_string_lossy().contains(".rsync-partial-"));
        assert!(partial.to_string_lossy().contains("file.txt"));
    }

    #[test]
    fn partial_destination_path_preserves_directory() {
        let dest = Path::new("/path/to/file.txt");
        let partial = partial_destination_path(dest);
        assert_eq!(partial.parent(), dest.parent());
    }

    #[test]
    fn partial_destination_path_handles_no_filename() {
        let dest = Path::new("/");
        let partial = partial_destination_path(dest);
        assert!(partial.to_string_lossy().contains("partial"));
    }

    #[test]
    fn temporary_destination_path_adds_prefix() {
        let dest = Path::new("/path/to/file.txt");
        let temp = temporary_destination_path(dest, 42, None);
        assert!(temp.to_string_lossy().contains(".rsync-tmp-"));
        assert!(temp.to_string_lossy().contains("file.txt"));
    }

    #[test]
    fn temporary_destination_path_includes_unique_id() {
        let dest = Path::new("/path/to/file.txt");
        let temp = temporary_destination_path(dest, 123, None);
        assert!(temp.to_string_lossy().contains("123"));
    }

    #[test]
    fn temporary_destination_path_uses_temp_dir() {
        let dest = Path::new("/path/to/file.txt");
        let temp_dir = Path::new("/tmp/rsync");
        let temp = temporary_destination_path(dest, 1, Some(temp_dir));
        assert!(temp.starts_with(temp_dir));
    }

    #[test]
    fn temporary_destination_path_preserves_directory_without_temp_dir() {
        let dest = Path::new("/path/to/file.txt");
        let temp = temporary_destination_path(dest, 1, None);
        assert_eq!(temp.parent(), dest.parent());
    }

    #[test]
    fn temporary_destination_path_handles_no_filename() {
        let dest = Path::new("/");
        let temp = temporary_destination_path(dest, 1, None);
        assert!(temp.to_string_lossy().contains("temp"));
    }
}
