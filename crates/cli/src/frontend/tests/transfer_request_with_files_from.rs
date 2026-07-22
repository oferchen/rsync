use super::common::*;
use super::*;

#[test]
fn transfer_request_with_files_from_uses_source_directory_for_relative_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("files-from-source");
    std::fs::create_dir(&source_dir).expect("create source");
    let nested = source_dir.join("nested");
    std::fs::create_dir(&nested).expect("create nested");

    let alpha_path = source_dir.join("alpha.txt");
    let beta_path = nested.join("beta.txt");
    std::fs::write(&alpha_path, b"alpha").expect("write alpha");
    std::fs::write(&beta_path, b"beta").expect("write beta");

    let list_path = tmp.path().join("files-from-relative.list");
    std::fs::write(&list_path, "alpha.txt\nnested/beta.txt\n").expect("write list");

    let dest_dir = tmp.path().join("files-from-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_alpha = dest_dir.join("alpha.txt");
    // --files-from implies --relative, so nested/beta.txt preserves its
    // directory structure at the destination.
    let copied_beta = dest_dir.join("nested").join("beta.txt");

    assert_eq!(std::fs::read(&copied_alpha).expect("read alpha"), b"alpha");
    assert_eq!(std::fs::read(&copied_beta).expect("read beta"), b"beta");
}

/// Verifies that `--files-from` only transfers listed files.
///
/// Upstream rsync reads filenames from `--files-from` and only includes those
/// in the file list. Unlisted files in the source directory must not be copied.
///
/// # Upstream Reference
///
/// - `flist.c:2275-2299` - `send_file_list()` reads from `filesfrom_fd`
/// - `options.c:2187-2195` - `--files-from` disables recursion, enables xfer_dirs
#[test]
fn files_from_excludes_unlisted_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("ff-exclude-src");
    std::fs::create_dir(&source_dir).expect("create source");

    // Create three files but only list two in the files-from file
    std::fs::write(source_dir.join("file1.txt"), b"content1").expect("write file1");
    std::fs::write(source_dir.join("file2.txt"), b"content2").expect("write file2");
    std::fs::write(source_dir.join("file3.txt"), b"content3").expect("write file3");

    let list_path = tmp.path().join("ff-exclude.list");
    std::fs::write(&list_path, "file1.txt\nfile2.txt\n").expect("write list");

    let dest_dir = tmp.path().join("ff-exclude-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    // Use -av to mirror the interop scenario args
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from(format!("--files-from={}", list_path.display())),
        OsString::from(format!("{}/", source_dir.display())),
        OsString::from(format!("{}/", dest_dir.display())),
    ]);

    assert_eq!(code, 0, "transfer should succeed");

    // Listed files must be present at the destination
    assert!(
        dest_dir.join("file1.txt").exists(),
        "file1.txt should be copied"
    );
    assert!(
        dest_dir.join("file2.txt").exists(),
        "file2.txt should be copied"
    );

    // Unlisted file must NOT be present at the destination
    assert!(
        !dest_dir.join("file3.txt").exists(),
        "file3.txt must not be copied - it is not in the --files-from list"
    );
}

/// Verifies that `--files-from` entries with embedded `/./` markers produce
/// the correct destination path structure.
///
/// Upstream rsync uses `/./` within a file list entry to split the path:
/// everything before becomes a chdir prefix (relative to the source argument)
/// and everything after becomes the transferred relative filename at the
/// destination.
///
/// # Upstream Reference
///
/// - `testsuite/files-from.test` - tests `from/./dir/subdir` entries
/// - `flist.c:2351-2353` - `strstr(fbuf, "/./")` splits at marker
#[test]
fn files_from_embedded_dot_marker_determines_destination_structure() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let scratch = tmp.path().join("scratch");

    // Create: scratch/from/dir/subdir/file.txt
    let subdir = scratch.join("from").join("dir").join("subdir");
    std::fs::create_dir_all(&subdir).expect("create subdir");
    std::fs::write(subdir.join("file.txt"), b"hello").expect("write file");

    // File list uses "from/./dir/subdir/file.txt" - the "./" marker means
    // "from" is the chdir prefix and "dir/subdir/file.txt" is the transfer name.
    let list_path = tmp.path().join("filelist");
    std::fs::write(&list_path, "from/./dir/subdir/file.txt\n").expect("write list");

    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).expect("create dest");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from(format!("--files-from={}", list_path.display())),
        scratch.clone().into_os_string(),
        OsString::from(format!("{}/", dest.display())),
    ]);

    assert_eq!(code, 0, "transfer should succeed");

    // The file should appear at dest/dir/subdir/file.txt (not dest/from/dir/...)
    // because "/./'' splits the path: "from" is just a chdir prefix.
    assert!(
        dest.join("dir").join("subdir").join("file.txt").exists(),
        "file should be at dir/subdir/file.txt, not from/dir/subdir/file.txt"
    );
    assert!(
        !dest.join("from").exists(),
        "the 'from' directory prefix should NOT appear at the destination"
    );
    assert_eq!(
        std::fs::read(dest.join("dir").join("subdir").join("file.txt")).expect("read"),
        b"hello"
    );
}

/// Verifies that `--files-from` entries flagged as DOTDIR_NAME (`from/./`) and
/// SLASH_ENDING_NAME (`dir/`) walk their immediate children even when global
/// recursion is off, matching upstream `flist.c:2477`.
///
/// Upstream's `(xfer_dirs && name_type != NORMAL_NAME)` predicate forces
/// `send_directory()` to emit one level of contents for the listed directory.
/// Subdirectories encountered during that walk are NOT recursed into further
/// (matching `recurse=0` semantics), but flat files and immediate subdirs are
/// transferred.
///
/// # Upstream Reference
///
/// - `testsuite/files-from.test` - the local invocation exercises this path
/// - `flist.c:2364` - SLASH_ENDING_NAME / DOTDIR_NAME flagging
/// - `flist.c:2477-2491` - `(xfer_dirs && name_type != NORMAL_NAME)` walk
#[test]
fn files_from_dotdir_entry_walks_immediate_children() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let scratch = tmp.path().join("scratch");
    let from = scratch.join("from");
    std::fs::create_dir_all(&from).expect("create from");

    // Flat files and an immediate subdir under from/.
    std::fs::write(from.join("alpha.txt"), b"alpha").expect("write alpha");
    std::fs::write(from.join("beta.txt"), b"beta").expect("write beta");
    let sub = from.join("sub");
    std::fs::create_dir(&sub).expect("create sub");
    // Files nested deeper than one level must NOT appear at the destination
    // because the DOTDIR walk is one level only.
    std::fs::write(sub.join("nested.txt"), b"nested").expect("write nested");

    // Filelist with a bare DOTDIR entry: should emit `from/`'s immediate
    // children at the destination root.
    let list_path = tmp.path().join("filelist");
    std::fs::write(&list_path, "from/./\n").expect("write list");

    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).expect("create dest");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from(format!("--files-from={}", list_path.display())),
        scratch.clone().into_os_string(),
        OsString::from(format!("{}/", dest.display())),
    ]);

    assert_eq!(code, 0, "transfer should succeed");

    // Flat children of `from/` appear at the destination root.
    assert!(
        dest.join("alpha.txt").exists(),
        "alpha.txt should appear under dest/"
    );
    assert!(
        dest.join("beta.txt").exists(),
        "beta.txt should appear under dest/"
    );
    // The immediate subdirectory must exist (one-level walk emits it).
    assert!(
        dest.join("sub").is_dir(),
        "sub/ should exist as an empty directory at dest/"
    );
    // Recursion stops at one level: the nested file must NOT be copied.
    assert!(
        !dest.join("sub").join("nested.txt").exists(),
        "sub/nested.txt must NOT be copied (recursion is off)"
    );
}

/// Verifies that a SLASH_ENDING_NAME `--files-from` entry (`dir/`) at a
/// deeper level walks its immediate children even when global recursion is
/// off.
///
/// Mirrors `testsuite/files-from.test` line 22 (`from/./dir/subdir/subsubdir2/`)
/// where the trailing slash must pull `bin-lt-list` into the destination.
#[test]
fn files_from_slash_ending_entry_walks_immediate_children() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let scratch = tmp.path().join("scratch");
    let leaf = scratch.join("from").join("dir").join("leaf");
    std::fs::create_dir_all(&leaf).expect("create leaf");
    std::fs::write(leaf.join("payload.bin"), b"payload").expect("write payload");

    let list_path = tmp.path().join("filelist");
    std::fs::write(&list_path, "from/./dir/leaf/\n").expect("write list");

    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).expect("create dest");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from(format!("--files-from={}", list_path.display())),
        scratch.clone().into_os_string(),
        OsString::from(format!("{}/", dest.display())),
    ]);

    assert_eq!(code, 0, "transfer should succeed");

    // The leaf directory must exist at dest/dir/leaf/ and contain the file
    // copied by the one-level walk.
    let copied = dest.join("dir").join("leaf").join("payload.bin");
    assert!(
        copied.exists(),
        "payload.bin should be copied via SLASH_ENDING_NAME walk"
    );
    assert_eq!(std::fs::read(&copied).expect("read"), b"payload");
}
