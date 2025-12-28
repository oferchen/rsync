use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering as AtomicOrdering;

use crate::local_copy::LocalCopyError;

use super::super::super::NEXT_TEMP_FILE_ID;
use super::paths::{
    partial_destination_path, partial_directory_destination_path, temporary_destination_path,
};

pub(crate) fn remove_existing_destination(path: &Path) -> Result<(), LocalCopyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LocalCopyError::io(
            "remove existing destination",
            path.to_path_buf(),
            error,
        )),
    }
}

pub(crate) fn remove_incomplete_destination(destination: &Path) {
    if let Err(error) = fs::remove_file(destination)
        && error.kind() != io::ErrorKind::NotFound
    {
        // Preserve the original error from the transfer attempt.
    }
}

pub(crate) struct DestinationWriteGuard {
    final_path: PathBuf,
    temp_path: PathBuf,
    preserve_on_error: bool,
    committed: bool,
}

impl DestinationWriteGuard {
    pub(crate) fn new(
        destination: &Path,
        partial: bool,
        partial_dir: Option<&Path>,
        temp_dir: Option<&Path>,
    ) -> Result<(Self, fs::File), LocalCopyError> {
        if partial {
            let temp_path = if let Some(dir) = partial_dir {
                partial_directory_destination_path(destination, dir)?
            } else {
                partial_destination_path(destination)
            };
            if let Err(error) = fs::remove_file(&temp_path)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(LocalCopyError::io(
                    "remove existing partial file",
                    temp_path,
                    error,
                ));
            }
            let file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)
                .map_err(|error| LocalCopyError::io("copy file", temp_path.clone(), error))?;
            Ok((
                Self {
                    final_path: destination.to_path_buf(),
                    temp_path,
                    preserve_on_error: true,
                    committed: false,
                },
                file,
            ))
        } else {
            loop {
                let unique = NEXT_TEMP_FILE_ID.fetch_add(1, AtomicOrdering::Relaxed);
                let temp_path = temporary_destination_path(destination, unique, temp_dir);
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp_path)
                {
                    Ok(file) => {
                        return Ok((
                            Self {
                                final_path: destination.to_path_buf(),
                                temp_path,
                                preserve_on_error: false,
                                committed: false,
                            },
                            file,
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        continue;
                    }
                    Err(error) => {
                        return Err(LocalCopyError::io("copy file", temp_path, error));
                    }
                }
            }
        }
    }

    pub(crate) fn staging_path(&self) -> &Path {
        &self.temp_path
    }

    pub(crate) fn commit(mut self) -> Result<(), LocalCopyError> {
        match fs::rename(&self.temp_path, &self.final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_existing_destination(&self.final_path)?;
                fs::rename(&self.temp_path, &self.final_path).map_err(|rename_error| {
                    LocalCopyError::io(self.finalise_action(), self.temp_path.clone(), rename_error)
                })?;
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                fs::copy(&self.temp_path, &self.final_path).map_err(|copy_error| {
                    LocalCopyError::io(self.finalise_action(), self.final_path.clone(), copy_error)
                })?;
                fs::remove_file(&self.temp_path).map_err(|remove_error| {
                    LocalCopyError::io(self.finalise_action(), self.temp_path.clone(), remove_error)
                })?;
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    self.finalise_action(),
                    self.temp_path.clone(),
                    error,
                ));
            }
        }
        self.committed = true;
        Ok(())
    }

    pub(crate) fn final_path(&self) -> &Path {
        &self.final_path
    }

    pub(crate) fn discard(mut self) {
        if self.preserve_on_error {
            self.committed = true;
            return;
        }

        if let Err(error) = fs::remove_file(&self.temp_path)
            && error.kind() != io::ErrorKind::NotFound
        {
            // Best-effort cleanup: the file may have been removed concurrently.
        }

        self.committed = true;
    }

    const fn finalise_action(&self) -> &'static str {
        if self.preserve_on_error {
            "finalise partial file"
        } else {
            "finalise temporary file"
        }
    }
}

impl Drop for DestinationWriteGuard {
    fn drop(&mut self) {
        if !self.committed && !self.preserve_on_error {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn remove_existing_destination_removes_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write file");

        let result = remove_existing_destination(&path);
        assert!(result.is_ok());
        assert!(!path.exists());
    }

    #[test]
    fn remove_existing_destination_succeeds_when_not_found() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");

        let result = remove_existing_destination(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn remove_incomplete_destination_removes_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("incomplete.txt");
        fs::write(&path, b"partial content").expect("write file");

        remove_incomplete_destination(&path);
        assert!(!path.exists());
    }

    #[test]
    fn remove_incomplete_destination_does_not_panic_when_not_found() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");

        // Should not panic
        remove_incomplete_destination(&path);
    }

    #[test]
    fn destination_write_guard_new_creates_temp_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, mut file) =
            DestinationWriteGuard::new(&dest, false, None, None).expect("guard");

        // Temp file should exist and be writable
        file.write_all(b"test content").expect("write");

        // Verify staging path is different from final path
        assert_ne!(guard.staging_path(), guard.final_path());
        assert!(guard.staging_path().exists());

        guard.discard();
    }

    #[test]
    fn destination_write_guard_commit_renames_to_final_path() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, mut file) =
            DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        file.write_all(b"content").expect("write");
        drop(file);

        let staging = guard.staging_path().to_path_buf();
        guard.commit().expect("commit");

        // Final path should exist
        assert!(dest.exists());
        // Staging path should be gone
        assert!(!staging.exists());

        // Verify content
        let content = fs::read_to_string(&dest).expect("read");
        assert_eq!(content, "content");
    }

    #[test]
    fn destination_write_guard_discard_removes_temp_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        let staging = guard.staging_path().to_path_buf();

        guard.discard();

        // Staging path should be removed
        assert!(!staging.exists());
        // Final path should not exist
        assert!(!dest.exists());
    }

    #[test]
    fn destination_write_guard_drop_removes_temp_file_if_not_committed() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let staging;
        {
            let (guard, _file) =
                DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
            staging = guard.staging_path().to_path_buf();
            // Guard is dropped here without commit
        }

        // Staging path should be removed by Drop
        assert!(!staging.exists());
    }

    #[test]
    fn destination_write_guard_partial_mode_creates_partial_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, mut file) = DestinationWriteGuard::new(&dest, true, None, None).expect("guard");

        file.write_all(b"partial content").expect("write");

        // Staging path should end with appropriate suffix for partial
        let staging = guard.staging_path().to_path_buf();
        assert!(staging.to_string_lossy().contains("final.txt"));

        guard.discard();
    }

    #[test]
    fn destination_write_guard_partial_preserves_on_discard() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, mut file) = DestinationWriteGuard::new(&dest, true, None, None).expect("guard");
        file.write_all(b"partial content").expect("write");
        drop(file);

        let staging = guard.staging_path().to_path_buf();
        guard.discard();

        // In partial mode, discard preserves the file
        assert!(staging.exists());
    }

    #[test]
    fn destination_write_guard_final_path_returns_destination() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");

        assert_eq!(guard.final_path(), dest.as_path());

        guard.discard();
    }

    #[test]
    fn destination_write_guard_commit_replaces_existing_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        // Create existing file
        fs::write(&dest, b"old content").expect("write existing");

        let (guard, mut file) =
            DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        file.write_all(b"new content").expect("write");
        drop(file);

        guard.commit().expect("commit");

        // Should have new content
        let content = fs::read_to_string(&dest).expect("read");
        assert_eq!(content, "new content");
    }

    #[test]
    fn destination_write_guard_staging_path_is_accessible() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");

        // Staging path should be a valid path we can access
        let staging = guard.staging_path();
        assert!(staging.exists());
        assert!(staging.is_file());

        guard.discard();
    }
}
