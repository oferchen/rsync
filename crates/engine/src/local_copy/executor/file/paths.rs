use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::local_copy::LocalCopyError;

pub(crate) fn partial_destination_path(destination: &Path) -> PathBuf {
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "partial".to_string());
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
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| OsStr::new("partial").to_os_string());
    Ok(base_dir.join(file_name))
}

pub(crate) fn temporary_destination_path(
    destination: &Path,
    unique: usize,
    temp_dir: Option<&Path>,
) -> PathBuf {
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "temp".to_string());
    let temp_name = format!(".rsync-tmp-{file_name}-{}-{}", process::id(), unique);
    match temp_dir {
        Some(dir) => dir.join(temp_name),
        None => destination.with_file_name(temp_name),
    }
}
