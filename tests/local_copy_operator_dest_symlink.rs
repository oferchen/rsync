//! A symlink the operator put in the destination path must not abort metadata.
//!
//! `oc-rsync -a src/ base/link/inner/` with `base/link -> real` is an ordinary
//! admin layout (`/backup -> /mnt/disk`). Upstream 3.5.0 enters the destination
//! once with a plain `change_dir()` (`main.c` `get_local_name()`) and resolves
//! each entry's parent relative to that cwd (`syscall.c:1245` `do_lchown_at()`
//! -> `secure_relative_open(NULL, dirpath, ...)`), so the operator's own path is
//! never re-walked and only names below the root are confined. It exits 0 with
//! every attribute preserved.
//!
//! oc used to walk the whole absolute parent path with `RESOLVE_NO_SYMLINKS`
//! for every path-based chown/utimes/chmod, which refused the operator's `link`
//! component: the data landed, but every directory, symlink and FIFO kept
//! "now" as its mtime, directories lost their setgid/sticky bits, and the run
//! exited 23 with `failed to preserve ownership ... Not a directory (20)`.
//! Regular files were unaffected because they take the fd-based path, which is
//! why the fixture carries every other kind.
//!
//! The `--keep-dirlinks` run always passed (it resolves parents through the
//! ambient namespace) and is kept as the control that the fixture itself is
//! sound. The opposite half of the rule - a symlink planted BELOW the root is
//! still refused - is pinned at the applier level in
//! `crates/metadata/tests/operator_dest_symlink_confinement.rs`, where the
//! swap can be staged deterministically.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::FileTime;

fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    PathBuf::from("oc-rsync")
}

/// 2001-09-09T01:46:40Z - far from "now", so a dropped utimes is unmistakable.
const OLD: i64 = 1_000_000_000;

fn old_time(offset: i64) -> FileTime {
    FileTime::from_unix_time(OLD + offset, 0)
}

/// Every directory in the fixture with its mtime offset; `""` is the root.
const DIRS: [(&str, i64); 5] = [
    ("d1/d2", 4),
    ("d1", 5),
    ("setgid", 6),
    ("sticky", 7),
    ("", 8),
];

fn lmtime(path: &Path) -> i64 {
    fs::symlink_metadata(path).expect("lstat").mtime()
}

/// Builds a source tree holding every node kind that takes the path-based
/// metadata route: nested directories (one setgid, one sticky), a symlink and
/// a FIFO. Timestamps are set deepest-first so no later creation disturbs a
/// parent's mtime.
fn seed_source(src: &Path) {
    fs::create_dir_all(src.join("d1/d2")).expect("mkdir src tree");
    fs::create_dir(src.join("setgid")).expect("mkdir setgid");
    fs::create_dir(src.join("sticky")).expect("mkdir sticky");
    fs::write(src.join("d1/d2/file"), b"payload").expect("write file");
    symlink("file", src.join("d1/d2/link")).expect("symlink");
    let status = Command::new("mkfifo")
        .arg(src.join("d1/d2/fifo"))
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo failed");

    fs::set_permissions(src.join("setgid"), fs::Permissions::from_mode(0o2755))
        .expect("chmod setgid");
    fs::set_permissions(src.join("sticky"), fs::Permissions::from_mode(0o1777))
        .expect("chmod sticky");

    filetime::set_file_times(src.join("d1/d2/file"), old_time(1), old_time(1)).expect("file mtime");
    filetime::set_symlink_file_times(src.join("d1/d2/link"), old_time(2), old_time(2))
        .expect("link mtime");
    // filetime's `set_file_times` opens the node, which blocks on a FIFO with
    // no writer. The symlink variant is an open-free `utimensat` with
    // `AT_SYMLINK_NOFOLLOW`, identical to a follow for a non-symlink.
    filetime::set_symlink_file_times(src.join("d1/d2/fifo"), old_time(3), old_time(3))
        .expect("fifo mtime");
    for (dir, offset) in DIRS {
        filetime::set_file_times(src.join(dir), old_time(offset), old_time(offset))
            .expect("dir mtime");
    }
}

/// Runs `oc-rsync -a [extra] src/ base/link/inner/` with `base/link -> real`
/// and returns where the data physically landed.
fn copy_through_operator_symlink(root: &Path, extra: &[&str], precreate_inner: bool) -> PathBuf {
    let src = root.join("src");
    seed_source(&src);
    let base = root.join("base");
    fs::create_dir_all(base.join("real")).expect("mkdir real");
    symlink("real", base.join("link")).expect("operator symlink");
    let landing = base.join("real/inner");
    if precreate_inner {
        fs::create_dir(&landing).expect("pre-create inner");
    }

    let mut dest = base.join("link/inner").into_os_string();
    dest.push("/");
    let mut src_arg = src.into_os_string();
    src_arg.push("/");
    let output = Command::new(oc_rsync_binary())
        .arg("-a")
        .args(extra)
        .arg(&src_arg)
        .arg(&dest)
        .output()
        .expect("run oc-rsync");
    assert!(
        output.status.success(),
        "upstream exits 0 here; oc exited {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    landing
}

/// Every path-based attribute must land, exactly as upstream leaves it.
fn assert_metadata_preserved(landing: &Path) {
    assert_eq!(
        fs::read(landing.join("d1/d2/file")).expect("read file"),
        b"payload",
        "the data must land through the operator's symlink",
    );
    assert_eq!(lmtime(&landing.join("d1/d2/file")), OLD + 1, "file mtime");
    assert_eq!(
        lmtime(&landing.join("d1/d2/link")),
        OLD + 2,
        "symlink mtime"
    );
    assert_eq!(lmtime(&landing.join("d1/d2/fifo")), OLD + 3, "fifo mtime");
    for (dir, offset) in DIRS {
        assert_eq!(
            lmtime(&landing.join(dir)),
            OLD + offset,
            "directory {dir:?} mtime must be preserved, not left at now",
        );
    }
    let mode = |p: &str| fs::metadata(landing.join(p)).expect("stat").mode() & 0o7777;
    assert_eq!(mode("sticky"), 0o1777, "sticky bit must be preserved");
    // An unprivileged macOS caller cannot set S_ISGID on a directory whose
    // group it is not in; `chmod(2)` masks it there. Linux grants it to the
    // owner, so only assert the bit where it is grantable.
    if cfg!(target_os = "linux") {
        assert_eq!(mode("setgid"), 0o2755, "setgid bit must be preserved");
    } else {
        assert_eq!(mode("setgid") & 0o777, 0o755, "setgid dir permissions");
    }
}

#[test]
fn copy_into_absent_dest_below_operator_symlink_preserves_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let landing = copy_through_operator_symlink(tmp.path(), &[], false);
    assert_metadata_preserved(&landing);
}

#[test]
fn copy_into_existing_dest_below_operator_symlink_preserves_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let landing = copy_through_operator_symlink(tmp.path(), &[], true);
    assert_metadata_preserved(&landing);
}

#[test]
fn keep_dirlinks_copy_below_operator_symlink_preserves_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let landing = copy_through_operator_symlink(tmp.path(), &["--keep-dirlinks"], false);
    assert_metadata_preserved(&landing);
}
