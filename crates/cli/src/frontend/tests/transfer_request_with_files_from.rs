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
/// - `flist.c:2240-2264` - `send_file_list()` reads from `filesfrom_fd`
/// - `options.c:2169-2177` - `--files-from` disables recursion, enables xfer_dirs
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
