use super::common::*;
use super::*;

/// Verifies that `rsync src/ dst/` without `-r`, `-d`, `-a`, or `--files-from`
/// skips the directory and writes nothing, matching upstream rsync 3.4.4.
///
/// Upstream's `recurse` defaults to `0` (options.c:112) and `xfer_dirs` falls
/// back to `0` when neither `-r` nor `-d` is supplied (options.c:2200-2203).
/// In `flist.c:2451`, a directory operand with `!xfer_dirs` triggers
/// `rprintf(FINFO, "skipping directory %s\n", fbuf)` and is omitted from the
/// flist, so the destination tree remains empty.
///
/// # Upstream Reference
///
/// - `options.c:112` - `int recurse = 0;` default
/// - `options.c:2200-2203` - `xfer_dirs = 0` when recurse is off and the user
///   did not request `-d`
/// - `flist.c:2451` - `S_ISDIR(st.st_mode) && !xfer_dirs` skips the directory
#[test]
fn trailing_slash_source_without_recursion_skips_directory() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    std::fs::create_dir(&source_dir).expect("create source");
    std::fs::write(source_dir.join("child.txt"), b"payload").expect("write child");

    let dest_dir = tmp.path().join("dst");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push("/");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-times"),
        OsString::from("--no-perms"),
        source_arg,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "skipping a top-level dir is a success");
    assert!(
        !dest_dir.exists()
            || std::fs::read_dir(&dest_dir)
                .expect("read dst")
                .next()
                .is_none(),
        "destination must be absent or empty when recursion is disabled, got entries at {}",
        dest_dir.display()
    );
}

/// Verifies that `rsync src dst` (no trailing slash) without recursion skips
/// the source directory entirely.
///
/// Same upstream semantics as the trailing-slash variant: the directory operand
/// hits the `!xfer_dirs` guard in `flist.c:2451` and is omitted from the flist.
#[test]
fn non_trailing_slash_source_without_recursion_skips_directory() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    std::fs::create_dir(&source_dir).expect("create source");
    std::fs::write(source_dir.join("child.txt"), b"payload").expect("write child");

    let dest_dir = tmp.path().join("dst");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-times"),
        OsString::from("--no-perms"),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        !dest_dir.exists(),
        "destination must not be created when the only source is a skipped directory"
    );
}

/// Verifies that `-r` re-enables the recursive copy that the default would have
/// skipped. Guards against an over-broad fix that disables recursion entirely.
#[test]
fn trailing_slash_source_with_recursion_copies_contents() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let nested = source_dir.join("nested");
    std::fs::create_dir_all(&nested).expect("create nested");
    std::fs::write(source_dir.join("top.txt"), b"top").expect("write top");
    std::fs::write(nested.join("inner.txt"), b"inner").expect("write inner");

    let dest_dir = tmp.path().join("dst");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push("/");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        OsString::from("--no-times"),
        OsString::from("--no-perms"),
        source_arg,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read(dest_dir.join("top.txt")).expect("read top"),
        b"top"
    );
    assert_eq!(
        std::fs::read(dest_dir.join("nested").join("inner.txt")).expect("read inner"),
        b"inner"
    );
}

/// Verifies that `-d` walks one level of children but does not recurse,
/// matching upstream's `xfer_dirs && !recurse` behaviour.
#[test]
fn trailing_slash_source_with_dirs_only_walks_one_level() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let nested = source_dir.join("nested");
    std::fs::create_dir_all(&nested).expect("create nested");
    std::fs::write(source_dir.join("top.txt"), b"top").expect("write top");
    std::fs::write(nested.join("inner.txt"), b"inner").expect("write inner");

    let dest_dir = tmp.path().join("dst");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push("/");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-d"),
        OsString::from("--no-times"),
        OsString::from("--no-perms"),
        source_arg,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read(dest_dir.join("top.txt")).expect("read top"),
        b"top"
    );
    assert!(
        !dest_dir.join("nested").join("inner.txt").exists(),
        "nested children must not be copied with -d alone"
    );
}
