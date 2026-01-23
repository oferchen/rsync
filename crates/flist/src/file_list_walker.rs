use crate::entry::FileListEntry;
use crate::error::FileListError;
use logging::debug_log;
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

/// Depth-first iterator over filesystem entries.
pub struct FileListWalker {
    pub(crate) root: PathBuf,
    pub(crate) follow_symlinks: bool,
    pub(crate) yielded_root: bool,
    pub(crate) root_metadata: Option<fs::Metadata>,
    pub(crate) stack: Vec<DirectoryState>,
    pub(crate) visited: HashSet<PathBuf>,
    pub(crate) finished: bool,
}

impl FileListWalker {
    pub(crate) fn new(
        root: PathBuf,
        follow_symlinks: bool,
        include_root: bool,
    ) -> Result<Self, FileListError> {
        let root = absolutize(root)?;
        debug_log!(Flist, 1, "building file list from {:?}", root);

        let metadata = fs::symlink_metadata(&root)
            .map_err(|error| FileListError::root_metadata(root.clone(), error))?;

        let mut walker = Self {
            root,
            follow_symlinks,
            yielded_root: !include_root,
            root_metadata: Some(metadata),
            stack: Vec::new(),
            visited: HashSet::new(),
            finished: false,
        };

        if let Some(metadata) = walker.root_metadata.as_ref() {
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                walker.push_directory(walker.root.clone(), PathBuf::new(), 0)?;
            } else if file_type.is_symlink() && walker.follow_symlinks {
                match fs::metadata(&walker.root) {
                    Ok(target) if target.is_dir() => {
                        walker.push_directory(walker.root.clone(), PathBuf::new(), 0)?;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        return Err(FileListError::metadata(walker.root.clone(), error));
                    }
                }
            }
        }

        Ok(walker)
    }

    fn push_directory(
        &mut self,
        fs_path: PathBuf,
        relative_prefix: PathBuf,
        depth: usize,
    ) -> Result<(), FileListError> {
        let canonical = fs::canonicalize(&fs_path)
            .map_err(|error| FileListError::canonicalize(fs_path.clone(), error))?;
        if !self.visited.insert(canonical) {
            debug_log!(Dup, 1, "skipping already visited directory: {:?}", fs_path);
            return Ok(());
        }

        debug_log!(Flist, 3, "entering directory: {:?}", fs_path);
        let state = DirectoryState::new(fs_path, relative_prefix, depth)?;
        self.stack.push(state);
        Ok(())
    }

    fn prepare_entry(
        &mut self,
        full_path: PathBuf,
        relative_path: PathBuf,
        depth: usize,
    ) -> Result<FileListEntry, FileListError> {
        debug_log!(Flist, 4, "processing entry: {:?}", relative_path);

        let metadata = fs::symlink_metadata(&full_path)
            .map_err(|error| FileListError::metadata(full_path.clone(), error))?;
        let mut next_state = None;

        if metadata.file_type().is_dir() {
            next_state = Some((full_path.clone(), relative_path.clone(), depth));
        } else if metadata.file_type().is_symlink() && self.follow_symlinks {
            match fs::metadata(&full_path) {
                Ok(target) if target.is_dir() => {
                    let canonical = fs::canonicalize(&full_path)
                        .map_err(|error| FileListError::canonicalize(full_path.clone(), error))?;
                    next_state = Some((canonical, relative_path.clone(), depth));
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(FileListError::metadata(full_path, error));
                }
            }
        }

        if let Some((dir_path, rel_prefix, dir_depth)) = next_state {
            self.push_directory(dir_path, rel_prefix, dir_depth)?;
        }

        Ok(FileListEntry {
            full_path,
            relative_path,
            metadata,
            depth,
            is_root: false,
        })
    }
}

impl Iterator for FileListWalker {
    type Item = Result<FileListEntry, FileListError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        if !self.yielded_root {
            self.yielded_root = true;
            if let Some(metadata) = self.root_metadata.take() {
                let entry = FileListEntry {
                    full_path: self.root.clone(),
                    relative_path: PathBuf::new(),
                    metadata,
                    depth: 0,
                    is_root: true,
                };
                return Some(Ok(entry));
            }
        }

        loop {
            let (full_path, relative_path, depth) = {
                let state = self.stack.last_mut()?;

                if let Some(name) = state.next_name() {
                    let full_path = state.fs_path.join(&name);
                    // Use join() which is equivalent to clone()+push() but clearer.
                    // When relative_prefix is empty, join() with name creates PathBuf from name.
                    let relative_path = state.relative_prefix.join(&name);
                    (full_path, relative_path, state.depth + 1)
                } else {
                    self.stack.pop();
                    continue;
                }
            };

            match self.prepare_entry(full_path, relative_path, depth) {
                Ok(entry) => return Some(Ok(entry)),
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DirectoryState {
    fs_path: PathBuf,
    relative_prefix: PathBuf,
    entries: Vec<OsString>,
    index: usize,
    depth: usize,
}

impl DirectoryState {
    fn new(
        fs_path: PathBuf,
        relative_prefix: PathBuf,
        depth: usize,
    ) -> Result<Self, FileListError> {
        let mut entries = Vec::new();
        let read_dir = fs::read_dir(&fs_path)
            .map_err(|error| FileListError::read_dir(fs_path.clone(), error))?;
        for entry in read_dir {
            let entry =
                entry.map_err(|error| FileListError::read_dir_entry(fs_path.clone(), error))?;
            entries.push(entry.file_name());
        }
        entries.sort();

        debug_log!(Flist, 3, "found {} entries in {:?}", entries.len(), fs_path);

        Ok(Self {
            fs_path,
            relative_prefix,
            entries,
            index: 0,
            depth,
        })
    }

    /// Returns the next entry name, taking ownership to avoid cloning.
    fn next_name(&mut self) -> Option<OsString> {
        if self.index < self.entries.len() {
            // Take ownership of the entry to avoid cloning.
            // We replace with empty OsString which is cheap (no allocation).
            let name = std::mem::take(&mut self.entries[self.index]);
            self.index += 1;
            Some(name)
        } else {
            None
        }
    }
}

fn absolutize(path: PathBuf) -> Result<PathBuf, FileListError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        let cwd = env::current_dir()
            .map_err(|error| FileListError::canonicalize(PathBuf::from("."), error))?;
        Ok(cwd.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== absolutize tests ====================

    #[test]
    fn absolutize_returns_absolute_path_unchanged() {
        let path = PathBuf::from("/some/absolute/path");
        let result = absolutize(path.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), path);
    }

    #[test]
    fn absolutize_returns_absolute_path_unchanged_root() {
        let path = PathBuf::from("/");
        let result = absolutize(path.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), path);
    }

    #[test]
    fn absolutize_converts_relative_path_to_absolute() {
        let path = PathBuf::from("relative/path");
        let result = absolutize(path);
        assert!(result.is_ok());
        let abs = result.unwrap();
        assert!(abs.is_absolute());
        assert!(abs.ends_with("relative/path"));
    }

    #[test]
    fn absolutize_handles_dot_path() {
        let path = PathBuf::from(".");
        let result = absolutize(path);
        assert!(result.is_ok());
        let abs = result.unwrap();
        assert!(abs.is_absolute());
    }

    #[test]
    fn absolutize_handles_empty_path() {
        let path = PathBuf::from("");
        let result = absolutize(path);
        assert!(result.is_ok());
        let abs = result.unwrap();
        assert!(abs.is_absolute());
    }

    // ==================== DirectoryState::next_name tests ====================

    #[test]
    fn directory_state_next_name_returns_none_when_empty() {
        let mut state = DirectoryState {
            fs_path: PathBuf::from("/test"),
            relative_prefix: PathBuf::new(),
            entries: Vec::new(),
            index: 0,
            depth: 0,
        };
        assert!(state.next_name().is_none());
    }

    #[test]
    fn directory_state_next_name_returns_entries_in_order() {
        let mut state = DirectoryState {
            fs_path: PathBuf::from("/test"),
            relative_prefix: PathBuf::new(),
            entries: vec![
                OsString::from("a"),
                OsString::from("b"),
                OsString::from("c"),
            ],
            index: 0,
            depth: 0,
        };
        assert_eq!(state.next_name(), Some(OsString::from("a")));
        assert_eq!(state.next_name(), Some(OsString::from("b")));
        assert_eq!(state.next_name(), Some(OsString::from("c")));
        assert!(state.next_name().is_none());
    }

    #[test]
    fn directory_state_next_name_advances_index() {
        let mut state = DirectoryState {
            fs_path: PathBuf::from("/test"),
            relative_prefix: PathBuf::new(),
            entries: vec![OsString::from("first"), OsString::from("second")],
            index: 0,
            depth: 0,
        };
        let _ = state.next_name();
        assert_eq!(state.index, 1);
        let _ = state.next_name();
        assert_eq!(state.index, 2);
    }

    #[test]
    fn directory_state_next_name_returns_none_after_exhaustion() {
        let mut state = DirectoryState {
            fs_path: PathBuf::from("/test"),
            relative_prefix: PathBuf::new(),
            entries: vec![OsString::from("only")],
            index: 0,
            depth: 0,
        };
        assert_eq!(state.next_name(), Some(OsString::from("only")));
        assert!(state.next_name().is_none());
        assert!(state.next_name().is_none()); // Repeated calls still return None
    }

    #[test]
    fn directory_state_clone() {
        let state = DirectoryState {
            fs_path: PathBuf::from("/test"),
            relative_prefix: PathBuf::from("rel"),
            entries: vec![OsString::from("entry")],
            index: 0,
            depth: 5,
        };
        let cloned = state.clone();
        assert_eq!(cloned.fs_path, state.fs_path);
        assert_eq!(cloned.relative_prefix, state.relative_prefix);
        assert_eq!(cloned.depth, state.depth);
    }

    #[test]
    fn directory_state_debug() {
        let state = DirectoryState {
            fs_path: PathBuf::from("/test"),
            relative_prefix: PathBuf::new(),
            entries: Vec::new(),
            index: 0,
            depth: 0,
        };
        let debug = format!("{state:?}");
        assert!(debug.contains("DirectoryState"));
        assert!(debug.contains("/test"));
    }

    // ==================== FileListWalker tests with filesystem ====================

    #[test]
    fn file_list_walker_walks_temp_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write");

        let walker =
            FileListWalker::new(temp.path().to_path_buf(), false, true).expect("create walker");

        let entries: Vec<_> = walker.collect();
        assert!(!entries.is_empty());

        // Should have at least the root directory and one file
        let mut found_file = false;
        for result in entries {
            let entry = result.expect("entry");
            if entry.relative_path.to_string_lossy().contains("test.txt") {
                found_file = true;
            }
        }
        assert!(found_file);
    }

    #[test]
    fn file_list_walker_empty_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let walker =
            FileListWalker::new(temp.path().to_path_buf(), false, true).expect("create walker");

        let entries: Vec<_> = walker.collect();
        // Should have just the root directory
        assert_eq!(entries.len(), 1);
        let entry = entries[0].as_ref().expect("entry");
        assert!(entry.is_root);
    }

    #[test]
    fn file_list_walker_single_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("single.txt");
        std::fs::write(&file_path, b"content").expect("write");

        let walker = FileListWalker::new(file_path.clone(), false, true).expect("create walker");

        let entries: Vec<_> = walker.collect();
        assert_eq!(entries.len(), 1);
        let entry = entries[0].as_ref().expect("entry");
        assert!(entry.is_root);
        assert_eq!(entry.full_path, file_path);
    }
}
