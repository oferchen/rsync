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

/// Compare two destination trees for entry set, type, file bytes, and symlink
/// targets. Returns the first divergence, or `None` when identical.
pub fn content_diff(a: &Path, b: &Path) -> Option<String> {
    let (ea, eb) = (rel_entries(a), rel_entries(b));
    if ea != eb {
        return Some(format!(
            "entry set differs ({} vs {} entries)",
            ea.len(),
            eb.len()
        ));
    }
    for rel in &ea {
        let (pa, pb) = (a.join(rel), b.join(rel));
        let (ma, mb) = match (pa.symlink_metadata(), pb.symlink_metadata()) {
            (Ok(ma), Ok(mb)) => (ma, mb),
            _ => return Some(format!("cannot stat {}", rel.display())),
        };
        let (ta, tb) = (ma.file_type(), mb.file_type());
        if ta.is_symlink() != tb.is_symlink() || ta.is_dir() != tb.is_dir() {
            return Some(format!("type differs at {}", rel.display()));
        }
        if ta.is_symlink() {
            if std::fs::read_link(&pa).ok() != std::fs::read_link(&pb).ok() {
                return Some(format!("symlink target differs at {}", rel.display()));
            }
        } else if ta.is_file() && std::fs::read(&pa).ok() != std::fs::read(&pb).ok() {
            return Some(format!("file content differs at {}", rel.display()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{content_diff, entry_count, rel_entries};
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

    #[test]
    fn content_diff_none_for_identical_trees() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for root in [a.path(), b.path()] {
            fs::write(root.join("f"), b"same").unwrap();
            symlink("f", root.join("l")).unwrap();
        }
        assert!(content_diff(a.path(), b.path()).is_none());
    }

    #[test]
    fn content_diff_reports_content_symlink_and_set_differences() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(a.path().join("f"), b"alpha").unwrap();
        fs::write(b.path().join("f"), b"beta").unwrap();
        assert!(
            content_diff(a.path(), b.path())
                .unwrap()
                .contains("content differs")
        );

        let (c, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        symlink("x", c.path().join("l")).unwrap();
        symlink("y", d.path().join("l")).unwrap();
        assert!(
            content_diff(c.path(), d.path())
                .unwrap()
                .contains("symlink target")
        );

        let (e, f) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(e.path().join("only"), b"").unwrap();
        assert!(
            content_diff(e.path(), f.path())
                .unwrap()
                .contains("entry set")
        );
    }
}
