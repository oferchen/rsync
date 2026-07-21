//! Shared helpers for validate checks: external-tool capture, tool probing,
//! entry counting, and content comparison.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{TaskError, TaskResult};

/// Run `program args...`, returning trimmed stdout; error on non-zero exit.
pub fn capture(program: &str, args: &[&str]) -> TaskResult<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| TaskError::Validation(format!("spawn {program}: {e}")))?;
    if !out.status.success() {
        return Err(TaskError::Validation(format!(
            "{program} {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// True if a TCP connection to localhost:22 succeeds (sshd likely present).
pub fn ssh_ready() -> bool {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 22);
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
}

/// True if `program` can be spawned (present on PATH).
pub fn tool_available(program: &str) -> bool {
    Command::new(program)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Recursively count entries (files, dirs, symlinks) under `dir`.
pub fn entry_count(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            count += 1;
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                count += entry_count(&entry.path());
            }
        }
    }
    count
}

/// Sorted relative paths of every entry under `root` (dirs, files, symlinks).
pub fn rel_entries(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(root, root, &mut out);
    out.sort();
    out
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                collect(root, &path, out);
            }
        }
    }
}

/// The content facet lives in [`super::comparison`]; re-exported here so the
/// many checks that reach for `support::content_diff` keep one import path.
pub use super::comparison::content_diff;

#[cfg(test)]
mod tests {
    use super::{entry_count, rel_entries};
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn entry_count_recurses_but_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("a"), b"a").unwrap();
        fs::write(root.join("sub/b"), b"b").unwrap();
        symlink("a", root.join("link")).unwrap();
        // sub + a + sub/b + link = 4; the symlink is counted but not traversed.
        assert_eq!(entry_count(root), 4);
    }

    #[test]
    fn rel_entries_are_sorted_and_relative() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("z")).unwrap();
        fs::write(dir.path().join("a"), b"").unwrap();
        fs::write(dir.path().join("z/y"), b"").unwrap();
        let rels: Vec<String> = rel_entries(dir.path())
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(rels, vec!["a", "z", "z/y"]);
    }
}
