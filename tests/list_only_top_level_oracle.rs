//! `--list-only` without `-r` lists the top level once and never consults a
//! destination.
//!
//! Upstream resolves `xfer_dirs` to 1 for a bare `--list-only`
//! (`options.c:2324-2329`), so a `dir/` operand contributes its own "." and
//! one level of children and a subdirectory is listed, not descended. The
//! destination operand is inert: `get_local_name()` returns NULL under
//! `list_only` (`main.c:725`) and `recv_generator()` prints each entry and
//! returns before any destination lookup (`generator.c:1638-1643`). Several
//! `dir/` operands fold into one sorted file list, so the "." row appears
//! once (`flist.c:send_file_list()` + `flist_sort_and_clean()`).
//!
//! Every expected listing below is the verbatim stdout of rsync 3.5.1 on the
//! identical fixture with `TZ=UTC`. The only normalisation is the size column
//! of directory rows, which is the filesystem's `st_size` for a directory
//! (4,096 on the ext4 host the oracle ran on) and not portable.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::{FileTime, set_file_mtime};

/// 2020-01-02 03:04:05 UTC.
const FIXTURE_MTIME: i64 = 1_577_934_245;

const DOT_ONE_LEVEL: &str = "\
drwxr-xr-x          4,096 2020/01/02 03:04:05 .
-rw-r--r--              6 2020/01/02 03:04:05 a.txt
drwxr-xr-x          4,096 2020/01/02 03:04:05 sub
";

/// Builds `src/{a.txt, sub/{b.txt, deep/c.txt}}` with fixed modes and mtimes.
fn fixture(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub/deep")).expect("create tree");
    for (path, body) in [
        ("a.txt", "hello\n"),
        ("sub/b.txt", "world\n"),
        ("sub/deep/c.txt", "x\n"),
    ] {
        let file = src.join(path);
        fs::write(&file, body).expect("write file");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod file");
    }
    // Children before parents so setting a child's mtime cannot bump its parent.
    for dir in ["sub/deep", "sub", ""] {
        let path = src.join(dir);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod dir");
    }
    let mtime = FileTime::from_unix_time(FIXTURE_MTIME, 0);
    for path in [
        "a.txt",
        "sub/b.txt",
        "sub/deep/c.txt",
        "sub/deep",
        "sub",
        "",
    ] {
        set_file_mtime(src.join(path), mtime).expect("set mtime");
    }
}

/// Blanks the size column of directory rows (see the module docs).
fn normalise(listing: &str) -> String {
    listing
        .lines()
        .map(|line| match line.split_once(' ') {
            Some((perms, rest)) if perms.starts_with('d') => {
                let rest = rest.trim_start();
                let (_, tail) = rest.split_once(' ').expect("size column");
                format!("{perms} <dir-size> {tail}")
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn list_only(cwd: &Path, operands: &[&str]) -> (i32, String) {
    let output = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync")))
        .arg("--list-only")
        .args(operands)
        .current_dir(cwd)
        .env("TZ", "UTC")
        .output()
        .expect("run oc-rsync");
    assert!(
        output.stderr.is_empty(),
        "upstream prints nothing on stderr for {operands:?}, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).expect("utf-8 listing"),
    )
}

fn assert_listing(operands: &[&str], expected: &str) {
    let temp = tempfile::tempdir().expect("tempdir");
    fixture(temp.path());
    let (code, stdout) = list_only(temp.path(), operands);
    assert_eq!(code, 0, "exit code for {operands:?}");
    assert_eq!(
        normalise(&stdout),
        normalise(expected),
        "listing for {operands:?}"
    );
    assert!(
        !temp.path().join("dst").exists(),
        "a listing must not create the destination"
    );
}

#[test]
fn trailing_slash_source_lists_one_level_without_descending() {
    assert_listing(&["src/"], DOT_ONE_LEVEL);
}

#[test]
fn bare_directory_source_lists_only_itself() {
    assert_listing(
        &["src"],
        "drwxr-xr-x          4,096 2020/01/02 03:04:05 src\n",
    );
}

#[test]
fn missing_destination_is_neither_consulted_nor_created() {
    assert_listing(&["src/", "dst"], DOT_ONE_LEVEL);
}

/// A destination inside the source must not hide the entry it names: no
/// destination is ever created, so there is no output tree to exclude.
#[test]
fn destination_inside_source_does_not_hide_that_entry() {
    assert_listing(&["src/", "src/sub"], DOT_ONE_LEVEL);
}

/// A destination that is a regular file is never inspected, so it can neither
/// fail the run nor change what is listed.
#[test]
fn destination_that_is_a_file_is_never_inspected() {
    assert_listing(&["src/", "src/a.txt"], DOT_ONE_LEVEL);
}

/// Two `dir/` operands fold into one sorted list with a single "." row.
#[test]
fn several_contents_operands_merge_into_one_listing() {
    assert_listing(
        &["src/", "src/sub/", "dst"],
        "\
drwxr-xr-x          4,096 2020/01/02 03:04:05 .
-rw-r--r--              6 2020/01/02 03:04:05 a.txt
-rw-r--r--              6 2020/01/02 03:04:05 b.txt
drwxr-xr-x          4,096 2020/01/02 03:04:05 deep
drwxr-xr-x          4,096 2020/01/02 03:04:05 sub
",
    );
}
