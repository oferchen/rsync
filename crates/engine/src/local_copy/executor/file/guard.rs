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
    if let Err(error) = fs::remove_file(destination) {
        if error.kind() != io::ErrorKind::NotFound {
            // Preserve the original error from the transfer attempt.
        }
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
            if let Err(error) = fs::remove_file(&temp_path) {
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(LocalCopyError::io(
                        "remove existing partial file",
                        temp_path.clone(),
                        error,
                    ));
                }
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
                        return Err(LocalCopyError::io("copy file", temp_path.clone(), error));
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

        if let Err(error) = fs::remove_file(&self.temp_path) {
            if error.kind() != io::ErrorKind::NotFound {
                // Best-effort cleanup: the file may have been removed concurrently.
            }
        }

        self.committed = true;
    }

    fn finalise_action(&self) -> &'static str {
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
