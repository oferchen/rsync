//! Comparison logic for behavior testing.

use super::runner::{FileEntry, FileState, FileType, RunResult};
use super::scenarios::BehaviorScenario;
use std::path::PathBuf;

/// Differences found between two runs.
#[derive(Debug, Clone)]
pub enum Difference {
    /// Exit codes differ.
    ExitCode { oc_rsync: i32, upstream: i32 },

    /// File exists in one but not the other.
    FileMissing {
        path: PathBuf,
        in_oc_rsync: bool,
        in_upstream: bool,
    },

    /// File content differs.
    ContentDiffers {
        path: PathBuf,
        oc_rsync_size: u64,
        upstream_size: u64,
    },

    /// File type differs.
    FileTypeDiffers {
        path: PathBuf,
        oc_rsync: String,
        upstream: String,
    },

    /// Permission mode differs.
    PermissionsDiffer {
        path: PathBuf,
        oc_rsync: u32,
        upstream: u32,
    },

    /// Symlink target differs.
    SymlinkTargetDiffers {
        path: PathBuf,
        oc_rsync: PathBuf,
        upstream: PathBuf,
    },

    /// Hardlink structure differs.
    HardlinksDiffer { description: String },
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Difference::ExitCode { oc_rsync, upstream } => {
                write!(f, "Exit code: oc-rsync={}, upstream={}", oc_rsync, upstream)
            }
            Difference::FileMissing {
                path,
                in_oc_rsync,
                in_upstream,
            } => {
                if *in_oc_rsync && !*in_upstream {
                    write!(
                        f,
                        "File '{}' exists only in oc-rsync result",
                        path.display()
                    )
                } else if !*in_oc_rsync && *in_upstream {
                    write!(
                        f,
                        "File '{}' exists only in upstream result",
                        path.display()
                    )
                } else {
                    write!(f, "File '{}' missing status unclear", path.display())
                }
            }
            Difference::ContentDiffers {
                path,
                oc_rsync_size,
                upstream_size,
            } => {
                write!(
                    f,
                    "Content differs for '{}': oc-rsync={} bytes, upstream={} bytes",
                    path.display(),
                    oc_rsync_size,
                    upstream_size
                )
            }
            Difference::FileTypeDiffers {
                path,
                oc_rsync,
                upstream,
            } => {
                write!(
                    f,
                    "File type differs for '{}': oc-rsync={}, upstream={}",
                    path.display(),
                    oc_rsync,
                    upstream
                )
            }
            Difference::PermissionsDiffer {
                path,
                oc_rsync,
                upstream,
            } => {
                write!(
                    f,
                    "Permissions differ for '{}': oc-rsync={:o}, upstream={:o}",
                    path.display(),
                    oc_rsync & 0o7777,
                    upstream & 0o7777
                )
            }
            Difference::SymlinkTargetDiffers {
                path,
                oc_rsync,
                upstream,
            } => {
                write!(
                    f,
                    "Symlink target differs for '{}': oc-rsync={}, upstream={}",
                    path.display(),
                    oc_rsync.display(),
                    upstream.display()
                )
            }
            Difference::HardlinksDiffer { description } => {
                write!(f, "Hardlink structure differs: {}", description)
            }
        }
    }
}

/// Compare two run results based on scenario requirements.
pub fn compare_results(
    scenario: &BehaviorScenario,
    oc_rsync: &RunResult,
    upstream: &RunResult,
) -> Vec<Difference> {
    let mut differences = Vec::new();

    // Compare exit codes
    if scenario.compare_exit_code() && oc_rsync.exit_code != upstream.exit_code {
        differences.push(Difference::ExitCode {
            oc_rsync: oc_rsync.exit_code,
            upstream: upstream.exit_code,
        });
    }

    // Compare files
    if scenario.compare_files() {
        differences.extend(compare_file_states(
            &oc_rsync.files,
            &upstream.files,
            scenario,
        ));
    }

    differences
}

/// Compare file states between two runs.
fn compare_file_states(
    oc_rsync: &FileState,
    upstream: &FileState,
    scenario: &BehaviorScenario,
) -> Vec<Difference> {
    let mut differences = Vec::new();

    let oc_paths = oc_rsync.paths();
    let up_paths = upstream.paths();

    // Files in oc-rsync but not upstream
    for path in oc_paths.difference(&up_paths) {
        differences.push(Difference::FileMissing {
            path: path.clone(),
            in_oc_rsync: true,
            in_upstream: false,
        });
    }

    // Files in upstream but not oc-rsync
    for path in up_paths.difference(&oc_paths) {
        differences.push(Difference::FileMissing {
            path: path.clone(),
            in_oc_rsync: false,
            in_upstream: true,
        });
    }

    // Compare common files
    for path in oc_paths.intersection(&up_paths) {
        let oc_entry = oc_rsync.get(path).unwrap();
        let up_entry = upstream.get(path).unwrap();

        differences.extend(compare_entries(path, oc_entry, up_entry, scenario));
    }

    // Compare hardlink structure if requested
    if scenario.compare_hardlinks() {
        differences.extend(compare_hardlinks(oc_rsync, upstream));
    }

    differences
}

/// Compare individual file entries.
fn compare_entries(
    path: &std::path::Path,
    oc_entry: &FileEntry,
    up_entry: &FileEntry,
    scenario: &BehaviorScenario,
) -> Vec<Difference> {
    let mut differences = Vec::new();

    // Compare file types
    if oc_entry.file_type != up_entry.file_type {
        differences.push(Difference::FileTypeDiffers {
            path: path.to_path_buf(),
            oc_rsync: format!("{:?}", oc_entry.file_type),
            upstream: format!("{:?}", up_entry.file_type),
        });
        // If types differ, don't compare further
        return differences;
    }

    // Compare content for regular files
    if oc_entry.file_type == FileType::Regular {
        if oc_entry.content != up_entry.content {
            differences.push(Difference::ContentDiffers {
                path: path.to_path_buf(),
                oc_rsync_size: oc_entry.size,
                upstream_size: up_entry.size,
            });
        }
    }

    // Compare symlink targets
    if scenario.compare_symlinks() && oc_entry.file_type == FileType::Symlink {
        if oc_entry.symlink_target != up_entry.symlink_target {
            differences.push(Difference::SymlinkTargetDiffers {
                path: path.to_path_buf(),
                oc_rsync: oc_entry.symlink_target.clone().unwrap_or_default(),
                upstream: up_entry.symlink_target.clone().unwrap_or_default(),
            });
        }
    }

    // Compare permissions
    if scenario.compare_permissions() {
        // Only compare permission bits, not file type bits
        let oc_perms = oc_entry.mode & 0o7777;
        let up_perms = up_entry.mode & 0o7777;
        if oc_perms != up_perms {
            differences.push(Difference::PermissionsDiffer {
                path: path.to_path_buf(),
                oc_rsync: oc_entry.mode,
                upstream: up_entry.mode,
            });
        }
    }

    differences
}

/// Compare hardlink structure between two file states.
fn compare_hardlinks(oc_rsync: &FileState, upstream: &FileState) -> Vec<Difference> {
    let mut differences = Vec::new();

    let oc_groups = oc_rsync.hardlink_groups();
    let up_groups = upstream.hardlink_groups();

    // Normalize groups to sets of paths for comparison
    let oc_sets: std::collections::HashSet<_> = oc_groups
        .values()
        .map(|v| {
            let mut v = v.clone();
            v.sort();
            v
        })
        .collect();

    let up_sets: std::collections::HashSet<_> = up_groups
        .values()
        .map(|v| {
            let mut v = v.clone();
            v.sort();
            v
        })
        .collect();

    // Check for hardlink groups in oc-rsync but not upstream
    for group in oc_sets.difference(&up_sets) {
        differences.push(Difference::HardlinksDiffer {
            description: format!("Hardlink group {:?} exists only in oc-rsync result", group),
        });
    }

    // Check for hardlink groups in upstream but not oc-rsync
    for group in up_sets.difference(&oc_sets) {
        differences.push(Difference::HardlinksDiffer {
            description: format!("Hardlink group {:?} exists only in upstream result", group),
        });
    }

    differences
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_file_entry(content: &[u8], mode: u32) -> FileEntry {
        FileEntry {
            content: Some(content.to_vec()),
            file_type: FileType::Regular,
            mode,
            symlink_target: None,
            inode: 1,
            dev: 1,
            size: content.len() as u64,
        }
    }

    fn make_symlink_entry(target: &str) -> FileEntry {
        FileEntry {
            content: None,
            file_type: FileType::Symlink,
            mode: 0o777,
            symlink_target: Some(PathBuf::from(target)),
            inode: 1,
            dev: 1,
            size: 0,
        }
    }

    #[test]
    fn test_exit_code_comparison() {
        let scenario = BehaviorScenario {
            name: "test".to_string(),
            description: "test".to_string(),
            args: vec![],
            setup: None,
            compare: vec!["exit_code".to_string()],
            skip: false,
            known_difference: None,
            cleanup: None,
        };

        let oc_result = RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            files: FileState::default(),
        };

        let up_result = RunResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
            files: FileState::default(),
        };

        let diffs = compare_results(&scenario, &oc_result, &up_result);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0], Difference::ExitCode { .. }));
    }

    #[test]
    fn test_file_content_comparison() {
        let scenario = BehaviorScenario {
            name: "test".to_string(),
            description: "test".to_string(),
            args: vec![],
            setup: None,
            compare: vec!["files".to_string()],
            skip: false,
            known_difference: None,
            cleanup: None,
        };

        let mut oc_files = HashMap::new();
        oc_files.insert(
            PathBuf::from("file.txt"),
            make_file_entry(b"content A", 0o644),
        );

        let mut up_files = HashMap::new();
        up_files.insert(
            PathBuf::from("file.txt"),
            make_file_entry(b"content B", 0o644),
        );

        let oc_result = RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            files: FileState { entries: oc_files },
        };

        let up_result = RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            files: FileState { entries: up_files },
        };

        let diffs = compare_results(&scenario, &oc_result, &up_result);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0], Difference::ContentDiffers { .. }));
    }

    #[test]
    fn test_symlink_comparison() {
        let scenario = BehaviorScenario {
            name: "test".to_string(),
            description: "test".to_string(),
            args: vec![],
            setup: None,
            compare: vec!["files".to_string(), "symlinks".to_string()],
            skip: false,
            known_difference: None,
            cleanup: None,
        };

        let mut oc_files = HashMap::new();
        oc_files.insert(PathBuf::from("link"), make_symlink_entry("target_a"));

        let mut up_files = HashMap::new();
        up_files.insert(PathBuf::from("link"), make_symlink_entry("target_b"));

        let oc_result = RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            files: FileState { entries: oc_files },
        };

        let up_result = RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            files: FileState { entries: up_files },
        };

        let diffs = compare_results(&scenario, &oc_result, &up_result);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0], Difference::SymlinkTargetDiffers { .. }));
    }
}
