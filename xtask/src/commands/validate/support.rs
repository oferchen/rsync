//! Shared helpers for validate checks: external-tool capture, tool probing,
//! entry counting, and content comparison.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use filetime::FileTime;

use crate::error::{TaskError, TaskResult};

/// Run `program args...`, returning trimmed stdout; error on non-zero exit.
///
/// `touch` and `stat` are served from portable std/[`filetime`] implementations
/// instead of the host binary: the fixtures backdate mtimes with GNU
/// `touch -d @<epoch>` and read times with GNU `stat -c %W|%X`, flag forms that
/// BSD/macOS `touch` and `stat` reject. Shelling out there made every fixture
/// fail on macOS, collapsing whole checks into a single aggregate skip so their
/// remaining transport cells produced no outcome at all. The Rust
/// implementations behave identically to the GNU tools on the exact argument
/// shapes the harness uses, on every host, so the matrix reports real outcomes
/// everywhere. Every other program shells out unchanged.
pub fn capture(program: &str, args: &[&str]) -> TaskResult<String> {
    match program {
        "touch" => return portable_touch(args),
        "stat" => return portable_stat(args),
        _ => {}
    }
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

/// Portable stand-in for the GNU `touch` invocations the fixtures use.
///
/// Recognises the exact shape those call sites emit: optional `-h` (act on the
/// symlink itself), `-m` (mtime only), and `-a` (atime only) flags, a mandatory
/// `-d @<seconds>` epoch, and a trailing path. GNU `touch -d @<epoch>` with no
/// `-m`/`-a` sets *both* atime and mtime, which the default arm mirrors. Like
/// `touch`, a missing regular-file target is created first. Returns empty stdout
/// on success, matching what `touch` prints.
fn portable_touch(args: &[&str]) -> TaskResult<String> {
    let (mut no_deref, mut mtime_only, mut atime_only) = (false, false, false);
    let mut epoch: Option<i64> = None;
    let mut path: Option<&str> = None;

    let mut it = args.iter();
    while let Some(&arg) = it.next() {
        match arg {
            "-h" => no_deref = true,
            "-m" => mtime_only = true,
            "-a" => atime_only = true,
            "-d" => {
                let value = it.next().ok_or_else(|| {
                    TaskError::Validation("touch: -d requires a date argument".into())
                })?;
                epoch = Some(parse_epoch_arg(value)?);
            }
            other if other.starts_with('-') => {
                return Err(TaskError::Validation(format!(
                    "touch: unsupported flag `{other}` in portable stand-in"
                )));
            }
            other => path = Some(other),
        }
    }

    let epoch =
        epoch.ok_or_else(|| TaskError::Validation("touch: missing -d <date> argument".into()))?;
    let path = path.ok_or_else(|| TaskError::Validation("touch: missing path argument".into()))?;
    let path = Path::new(path);
    let ft = FileTime::from_unix_time(epoch, 0);

    // `touch` creates a missing file; `filetime` requires it to exist. A `-h`
    // request targets a symlink, which cannot be conjured, so only materialise
    // regular targets.
    if !no_deref && !path.exists() {
        std::fs::File::create(path)
            .map_err(|e| TaskError::Validation(format!("touch: create {}: {e}", path.display())))?;
    }

    let result = if no_deref {
        // The fixtures only pair `-h` with a full backdate (both times), never
        // with `-m`/`-a`; set both on the link itself to match.
        filetime::set_symlink_file_times(path, ft, ft)
    } else if mtime_only {
        filetime::set_file_mtime(path, ft)
    } else if atime_only {
        filetime::set_file_atime(path, ft)
    } else {
        filetime::set_file_times(path, ft, ft)
    };
    result
        .map(|()| String::new())
        .map_err(|e| TaskError::Validation(format!("touch: set times on {}: {e}", path.display())))
}

/// Parse a `touch -d` date argument. Only the `@<seconds>` epoch form the
/// fixtures use is accepted; anything else fails loudly rather than silently
/// stamping the wrong time.
fn parse_epoch_arg(value: &str) -> TaskResult<i64> {
    value
        .strip_prefix('@')
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| {
            TaskError::Validation(format!(
                "touch: portable stand-in only accepts `@<seconds>`, got `{value}`"
            ))
        })
}

/// Portable stand-in for the GNU `stat -c %W|%X <path>` invocations the fixtures
/// use. `%W` is birth time, `%X` access time, each printed as bare epoch
/// seconds. Symlinks are followed, mirroring `stat`'s default. When the
/// filesystem does not expose a birth time the value is reported as `0`, exactly
/// as GNU `stat` does, so the birth-time capability probe reads it the same way.
fn portable_stat(args: &[&str]) -> TaskResult<String> {
    let [flag, spec, path] = args else {
        return Err(TaskError::Validation(format!(
            "stat: portable stand-in expects `-c <spec> <path>`, got {args:?}"
        )));
    };
    if *flag != "-c" {
        return Err(TaskError::Validation(format!(
            "stat: portable stand-in only supports `-c`, got `{flag}`"
        )));
    }
    let path = Path::new(path);
    // Follow symlinks like `stat` (no `-L`/dereference distinction is needed
    // because no call site passes a symlink here, but matching the default keeps
    // the stand-in faithful).
    let meta = std::fs::metadata(path)
        .map_err(|e| TaskError::Validation(format!("stat: {}: {e}", path.display())))?;
    let secs = match *spec {
        "%X" => system_time_secs(meta.accessed().ok()),
        "%Y" => system_time_secs(meta.modified().ok()),
        // A filesystem without birth times reports 0, the GNU convention the
        // crtimes capability probe relies on.
        "%W" => system_time_secs(meta.created().ok()),
        other => {
            return Err(TaskError::Validation(format!(
                "stat: portable stand-in only supports %W/%X/%Y, got `{other}`"
            )));
        }
    };
    Ok(secs.to_string())
}

/// Whole seconds since the Unix epoch for `time`, or `0` when unavailable -
/// matching GNU `stat`'s `0` for an unexposed timestamp.
fn system_time_secs(time: Option<SystemTime>) -> i64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
    use super::{build_backdated_tree, capture, entry_count, parse_epoch_arg, rel_entries};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::UNIX_EPOCH;

    fn mtime_secs(path: &std::path::Path) -> i64 {
        fs::symlink_metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// The portable `touch` sets mtime from `-d @<epoch>` on every host, so the
    /// backdate the quick-check relies on works where BSD `touch -d` would fail.
    #[test]
    fn portable_touch_backdates_mtime_from_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        fs::write(&file, b"x").unwrap();
        capture(
            "touch",
            &["-h", "-d", "@1614830767", &file.to_string_lossy()],
        )
        .unwrap();
        assert_eq!(mtime_secs(&file), 1_614_830_767);
    }

    /// `-m` changes only the mtime, leaving the atime untouched, mirroring GNU
    /// `touch -m` - the separation the atimes fixture depends on.
    #[test]
    fn portable_touch_m_sets_only_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        fs::write(&file, b"x").unwrap();
        capture(
            "touch",
            &["-a", "-d", "@1500000000", &file.to_string_lossy()],
        )
        .unwrap();
        capture(
            "touch",
            &["-m", "-d", "@1614830767", &file.to_string_lossy()],
        )
        .unwrap();
        let meta = fs::metadata(&file).unwrap();
        let atime = meta
            .accessed()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(mtime_secs(&file), 1_614_830_767);
        assert_eq!(atime, 1_500_000_000, "-m must not disturb the atime");
    }

    /// `touch` creates a missing regular target, so a fixture that stamps a file
    /// it has not written yet still succeeds.
    #[test]
    fn portable_touch_creates_missing_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new");
        capture("touch", &["-d", "@1614830767", &file.to_string_lossy()]).unwrap();
        assert!(file.exists());
        assert_eq!(mtime_secs(&file), 1_614_830_767);
    }

    /// A non-`@` date is refused loudly rather than silently stamping "now".
    #[test]
    fn portable_touch_rejects_non_epoch_date() {
        assert!(parse_epoch_arg("2020-01-01 00:00:00").is_err());
        assert_eq!(parse_epoch_arg("@42").unwrap(), 42);
    }

    /// `stat -c %X` reads back the access time as bare epoch seconds, so the
    /// atime/crtime comparisons work without GNU `stat`.
    #[test]
    fn portable_stat_reads_access_time_as_epoch_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        fs::write(&file, b"x").unwrap();
        capture(
            "touch",
            &["-a", "-d", "@1500000000", &file.to_string_lossy()],
        )
        .unwrap();
        let got = capture("stat", &["-c", "%X", &file.to_string_lossy()]).unwrap();
        assert_eq!(got, "1500000000");
    }

    /// A missing path fails, so `stat`-based readers get `None` via `.ok()`.
    #[test]
    fn portable_stat_errors_on_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(capture("stat", &["-c", "%W", &missing.to_string_lossy()]).is_err());
    }

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
