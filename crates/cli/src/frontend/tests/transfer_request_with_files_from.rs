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
    let copied_beta = dest_dir.join("beta.txt");

    assert_eq!(std::fs::read(&copied_alpha).expect("read alpha"), b"alpha");
    assert_eq!(std::fs::read(&copied_beta).expect("read beta"), b"beta");
}
