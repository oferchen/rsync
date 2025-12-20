use crate::entry::WalkEntry;
use crate::error::WalkError;
use logging::debug_log;
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

/// Depth-first iterator over filesystem entries.
pub struct Walker {
    pub(crate) root: PathBuf,
    pub(crate) follow_symlinks: bool,
    pub(crate) yielded_root: bool,
    pub(crate) root_metadata: Option<fs::Metadata>,
    pub(crate) stack: Vec<DirectoryState>,
    pub(crate) visited: HashSet<PathBuf>,
    pub(crate) finished: bool,
}

impl Walker {
    pub(crate) fn new(
        root: PathBuf,
        follow_symlinks: bool,
        include_root: bool,
    ) -> Result<Self, WalkError> {
        let root = absolutize(root)?;
        debug_log!(Flist, 1, "building file list from {:?}", root);

        let metadata = fs::symlink_metadata(&root)
            .map_err(|error| WalkError::root_metadata(root.clone(), error))?;

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
                        return Err(WalkError::metadata(walker.root.clone(), error));
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
    ) -> Result<(), WalkError> {
        let canonical = fs::canonicalize(&fs_path)
            .map_err(|error| WalkError::canonicalize(fs_path.clone(), error))?;
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
    ) -> Result<WalkEntry, WalkError> {
        debug_log!(Flist, 4, "processing entry: {:?}", relative_path);

        let metadata = fs::symlink_metadata(&full_path)
            .map_err(|error| WalkError::metadata(full_path.clone(), error))?;
        let mut next_state = None;

        if metadata.file_type().is_dir() {
            next_state = Some((full_path.clone(), relative_path.clone(), depth));
        } else if metadata.file_type().is_symlink() && self.follow_symlinks {
            match fs::metadata(&full_path) {
                Ok(target) if target.is_dir() => {
                    let canonical = fs::canonicalize(&full_path)
                        .map_err(|error| WalkError::canonicalize(full_path.clone(), error))?;
                    next_state = Some((canonical, relative_path.clone(), depth));
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(WalkError::metadata(full_path.clone(), error));
                }
            }
        }

        if let Some((dir_path, rel_prefix, dir_depth)) = next_state {
            self.push_directory(dir_path, rel_prefix, dir_depth)?;
        }

        Ok(WalkEntry {
            full_path,
            relative_path,
            metadata,
            depth,
            is_root: false,
        })
    }
}

impl Iterator for Walker {
    type Item = Result<WalkEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        if !self.yielded_root {
            self.yielded_root = true;
            if let Some(metadata) = self.root_metadata.take() {
                let entry = WalkEntry {
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
                    let relative_path = if state.relative_prefix.as_os_str().is_empty() {
                        PathBuf::from(&name)
                    } else {
                        let mut rel = state.relative_prefix.clone();
                        rel.push(&name);
                        rel
                    };
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
    fn new(fs_path: PathBuf, relative_prefix: PathBuf, depth: usize) -> Result<Self, WalkError> {
        let mut entries = Vec::new();
        let read_dir =
            fs::read_dir(&fs_path).map_err(|error| WalkError::read_dir(fs_path.clone(), error))?;
        for entry in read_dir {
            let entry = entry.map_err(|error| WalkError::read_dir_entry(fs_path.clone(), error))?;
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

    fn next_name(&mut self) -> Option<OsString> {
        if let Some(name) = self.entries.get(self.index) {
            self.index += 1;
            Some(name.clone())
        } else {
            None
        }
    }
}

fn absolutize(path: PathBuf) -> Result<PathBuf, WalkError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        let cwd = env::current_dir()
            .map_err(|error| WalkError::canonicalize(PathBuf::from("."), error))?;
        Ok(cwd.join(path))
    }
}
