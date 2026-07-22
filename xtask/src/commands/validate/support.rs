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

/// Fixed past mtime stamped on every fixture entry and the source root.
const FIXTURE_MTIME: &str = "@1614830767";

/// Build the shared itemize/dry-run fixture (`a.txt`, `b.txt`, `sub/c.txt`) with
/// every entry *and the source root itself* backdated to a fixed past mtime.
///
/// Backdating the entries stops the quick-check from skipping a transfer.
/// Backdating the root matters just as much: a freshly-created destination
/// directory carries a "now" mtime, so a source root left at "now" makes the
/// `.d..t...... ./` top-directory itemize row appear only when the two land in
/// different clock seconds - a race that makes two sequentially-run clients'
/// plans compare unequal. A fixed old root mtime makes that row deterministic
/// (always present, identical) for every client.
pub fn build_backdated_tree(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    std::fs::write(src.join("a.txt"), b"alpha").map_err(|e| e.to_string())?;
    std::fs::write(src.join("b.txt"), b"bravo").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("c.txt"), b"charlie").map_err(|e| e.to_string())?;

    // Backdate the entries, then the root last: the writes above bumped the
    // root's mtime to "now", so it must be stamped after them.
    for entry in rel_entries(src) {
        let path = src.join(&entry);
        capture(
            "touch",
            &["-h", "-d", FIXTURE_MTIME, &path.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;
    }
    capture("touch", &["-d", FIXTURE_MTIME, &src.to_string_lossy()]).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_backdated_tree, entry_count, rel_entries};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::UNIX_EPOCH;

    #[test]
    fn backdated_tree_populates_and_backdates_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        build_backdated_tree(&src).unwrap();
        // a.txt + b.txt + sub + sub/c.txt = 4 entries.
        assert_eq!(entry_count(&src), 4);
        // The root mtime must be the fixed past value, not "now", so the
        // top-directory itemize row is deterministic across sequential clients.
        let secs = fs::metadata(&src)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(secs, 1_614_830_767);
    }

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
