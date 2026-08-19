use super::common::*;
use super::*;

/// Verifies that `rsync src/ dst/` without `-r`, `-d`, `-a`, or `--files-from`
/// skips the directory and writes nothing, matching upstream rsync 3.4.4.
///
/// Upstream's `recurse` defaults to `0` (options.c:1270) and `xfer_dirs` falls
/// back to `0` when neither `-r` nor `-d` is supplied (options.c:2200-2203).
/// In `flist.c:2452`, a directory operand with `!xfer_dirs` triggers
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

/// Runs `oc-rsync <flags> <src-dir> <dst>/` on a fresh tree and returns stdout.
///
/// The source is a bare directory operand with no `-r`/`-d`, which is the only
/// shape that reaches upstream's `!xfer_dirs` guard.
fn skip_notice_stdout(flags: &[&str]) -> String {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("subdir");
    std::fs::create_dir(&source_dir).expect("create source");
    std::fs::write(source_dir.join("child.txt"), b"payload").expect("write child");
    let dest_dir = tmp.path().join("dst");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let mut args = vec![OsString::from(RSYNC)];
    args.extend(flags.iter().map(OsString::from));
    args.push(source_dir.into_os_string());
    args.push(dest_dir.into_os_string());

    let (code, stdout, _stderr) = run_with_args(args);
    assert_eq!(code, 0, "skipping a directory operand is a success");
    String::from_utf8(stdout).expect("stdout is utf-8")
}

fn skip_notice_count(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|line| *line == "skipping directory subdir")
        .count()
}

/// The notice is printed at DEFAULT verbosity, and `--info=name0` does not
/// silence it.
///
/// upstream's condition is `!xfer_dirs` and nothing else - there is no
/// `INFO_GTE` and no verbosity test at either call site (flist.c:1484 in
/// `send_file_name`, flist.c:2724 in `send_file_list`). The only suppressor is
/// `quiet` inside `rwrite()` (log.c:344-345). Routing it through the NAME
/// output level, as the per-file listing is, hides it from every operator who
/// did not pass `-v` - and from one who explicitly disabled NAME, which is what
/// makes `--info=name0` the discriminating cell rather than a variation on the
/// default.
///
/// Measured against rsync 3.5.0: it prints the line in all three cells below.
#[test]
fn skipping_directory_prints_without_name_output() {
    for flags in [&[][..], &["--info=name0"][..], &["--info=skip1"][..]] {
        assert_eq!(
            skip_notice_count(&skip_notice_stdout(flags)),
            1,
            "`skipping directory subdir` must print exactly once with {flags:?}"
        );
    }
}

/// `-q` is the one thing that does suppress it, and the notice must not
/// duplicate at `-v`.
///
/// Without the `-q` half this pin would also pass on a renderer that simply
/// printed the line unconditionally, so the two cells are a pair: one proves
/// the notice escapes the NAME gate, the other proves it still answers to
/// upstream's actual suppressor.
#[test]
fn quiet_suppresses_the_notice_and_verbose_does_not_duplicate_it() {
    assert_eq!(
        skip_notice_count(&skip_notice_stdout(&["-q"])),
        0,
        "upstream's rwrite() returns early for FINFO under --quiet (log.c:344-345)"
    );
    assert_eq!(
        skip_notice_count(&skip_notice_stdout(&["-v"])),
        1,
        "-v must not add a second copy of the notice"
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
