//! Scenario execution engine for behavior comparison.

use super::scenarios::BehaviorScenario;
use crate::error::{TaskError, TaskResult};
use std::collections::{HashMap, HashSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of running a single scenario with one rsync implementation.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// Exit code from the rsync command.
    pub exit_code: i32,

    /// Stdout from the command (for output comparison).
    #[allow(dead_code)]
    pub stdout: String,

    /// Stderr from the command (for output comparison).
    #[allow(dead_code)]
    pub stderr: String,

    /// File states after the transfer.
    pub files: FileState,
}

/// Represents the state of files in a directory tree.
#[derive(Debug, Clone, Default)]
pub struct FileState {
    /// Map of relative path to file info.
    pub entries: HashMap<PathBuf, FileEntry>,
}

/// Information about a single file.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// File content (for regular files).
    pub content: Option<Vec<u8>>,

    /// File type (regular, directory, symlink).
    pub file_type: FileType,

    /// Permission mode.
    pub mode: u32,

    /// Symlink target (if symlink).
    pub symlink_target: Option<PathBuf>,

    /// Inode number (for hardlink detection).
    pub inode: u64,

    /// Device number (for hardlink detection).
    pub dev: u64,

    /// Size in bytes.
    pub size: u64,
}

/// File type enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    Other,
}

impl FileState {
    /// Collect file state from a directory.
    pub fn from_directory(base: &Path) -> std::io::Result<Self> {
        let mut entries = HashMap::new();
        collect_entries(base, base, &mut entries)?;
        Ok(Self { entries })
    }

    /// Get all file paths.
    pub fn paths(&self) -> HashSet<PathBuf> {
        self.entries.keys().cloned().collect()
    }

    /// Get entry for a path.
    pub fn get(&self, path: &Path) -> Option<&FileEntry> {
        self.entries.get(path)
    }

    /// Find hardlink groups (files with same inode).
    pub fn hardlink_groups(&self) -> HashMap<(u64, u64), Vec<PathBuf>> {
        let mut groups: HashMap<(u64, u64), Vec<PathBuf>> = HashMap::new();
        for (path, entry) in &self.entries {
            if entry.file_type == FileType::Regular {
                groups
                    .entry((entry.dev, entry.inode))
                    .or_default()
                    .push(path.clone());
            }
        }
        // Only keep groups with more than one file
        groups.retain(|_, paths| paths.len() > 1);
        groups
    }
}

fn collect_entries(
    base: &Path,
    current: &Path,
    entries: &mut HashMap<PathBuf, FileEntry>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path.strip_prefix(base).unwrap().to_path_buf();
        let metadata = entry.metadata()?;
        let symlink_metadata = fs::symlink_metadata(&path)?;

        let file_type = if symlink_metadata.file_type().is_symlink() {
            FileType::Symlink
        } else if metadata.is_dir() {
            FileType::Directory
        } else if metadata.is_file() {
            FileType::Regular
        } else {
            FileType::Other
        };

        let content = if file_type == FileType::Regular {
            Some(fs::read(&path)?)
        } else {
            None
        };

        let symlink_target = if file_type == FileType::Symlink {
            Some(fs::read_link(&path)?)
        } else {
            None
        };

        #[cfg(unix)]
        let (mode, inode, dev) = (
            metadata.mode(),
            symlink_metadata.ino(),
            symlink_metadata.dev(),
        );

        #[cfg(not(unix))]
        let (mode, inode, dev) = {
            let m = if metadata.permissions().readonly() {
                0o444
            } else {
                0o644
            };
            (m, 0u64, 0u64)
        };

        entries.insert(
            rel_path.clone(),
            FileEntry {
                content,
                file_type,
                mode,
                symlink_target,
                inode,
                dev,
                size: metadata.len(),
            },
        );

        if file_type == FileType::Directory {
            collect_entries(base, &path, entries)?;
        }
    }

    Ok(())
}

/// Options for running a scenario.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Enable verbose output.
    pub verbose: bool,

    /// Show stdout/stderr from commands.
    pub show_output: bool,
}

/// Execute a scenario with a specific rsync binary.
pub fn run_scenario(
    scenario: &BehaviorScenario,
    rsync_binary: &Path,
    work_dir: &Path,
    dest_subdir: &str,
    options: &RunOptions,
) -> TaskResult<RunResult> {
    // Create the destination directory for this run
    let dest_dir = work_dir.join(dest_subdir);
    fs::create_dir_all(&dest_dir).map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create dest dir: {}", e),
        ))
    })?;

    // Build the command arguments
    let mut cmd_args = scenario.args.clone();

    // Replace "rsync" with actual binary and "dest/" with actual dest path
    for arg in &mut cmd_args {
        if arg == "rsync" || arg.starts_with("rsync") {
            *arg = rsync_binary.to_string_lossy().to_string();
        } else if arg == "dest/" || arg.ends_with("dest/") {
            *arg = format!("{}/", dest_dir.display());
        } else if arg.starts_with("dest/") {
            let suffix = arg.strip_prefix("dest/").unwrap();
            *arg = format!("{}/{}", dest_dir.display(), suffix);
        }
    }

    // Also handle src/ paths
    let src_dir = work_dir.join("src");
    for arg in &mut cmd_args {
        if arg == "src/" || arg == "src" {
            *arg = format!("{}/", src_dir.display());
        } else if arg.starts_with("src/") {
            let suffix = arg.strip_prefix("src/").unwrap();
            *arg = format!("{}/{}", src_dir.display(), suffix);
        }
    }

    // Handle other file references in work_dir
    for arg in &mut cmd_args {
        // Handle files like filelist.txt that should be in work_dir
        if !arg.starts_with('/') && !arg.starts_with('-') {
            if let Some(file_name) = arg.strip_prefix("filelist") {
                *arg = format!("{}/filelist{}", work_dir.display(), file_name);
            }
        }
    }

    if options.verbose {
        eprintln!("[runner] Executing: {:?}", cmd_args);
    }

    // Execute the command
    let output = Command::new(&cmd_args[0])
        .args(&cmd_args[1..])
        .current_dir(work_dir)
        .output()
        .map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to execute rsync for '{}': {}", scenario.name, e),
            ))
        })?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if options.show_output {
        if !stdout.is_empty() {
            eprintln!("[runner] stdout:\n{}", stdout);
        }
        if !stderr.is_empty() {
            eprintln!("[runner] stderr:\n{}", stderr);
        }
    }

    // Collect file state from destination
    let files = if dest_dir.exists() {
        FileState::from_directory(&dest_dir).map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to collect file state: {}", e),
            ))
        })?
    } else {
        FileState::default()
    };

    Ok(RunResult {
        exit_code,
        stdout,
        stderr,
        files,
    })
}

/// Set up the test environment for a scenario.
pub fn setup_scenario(
    scenario: &BehaviorScenario,
    work_dir: &Path,
    options: &RunOptions,
) -> TaskResult<()> {
    // Create work directory
    fs::create_dir_all(work_dir).map_err(|e| {
        TaskError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create work dir: {}", e),
        ))
    })?;

    // Run setup commands if specified
    if let Some(ref setup) = scenario.setup {
        if options.verbose {
            eprintln!("[runner] Running setup: {}", setup);
        }

        let status = Command::new("bash")
            .arg("-c")
            .arg(setup)
            .current_dir(work_dir)
            .status()
            .map_err(|e| {
                TaskError::Io(std::io::Error::new(
                    e.kind(),
                    format!("Failed to run setup for '{}': {}", scenario.name, e),
                ))
            })?;

        if !status.success() && options.verbose {
            eprintln!(
                "[runner] Warning: setup exited with code {:?}",
                status.code()
            );
        }
    }

    Ok(())
}

/// Clean up after a scenario.
pub fn cleanup_scenario(
    scenario: &BehaviorScenario,
    work_dir: &Path,
    _options: &RunOptions,
) -> TaskResult<()> {
    // Run cleanup commands if specified
    if let Some(ref cleanup) = scenario.cleanup {
        let _ = Command::new("bash")
            .arg("-c")
            .arg(cleanup)
            .current_dir(work_dir)
            .status();
    }

    // Remove the work directory
    let _ = fs::remove_dir_all(work_dir);

    Ok(())
}
