use super::common::*;
use super::*;

#[test]
fn transfer_request_with_delete_excluded_prunes_filtered_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let dest_subdir = dest_root.join("source");
    std::fs::create_dir_all(&dest_subdir).expect("create destination contents");
    std::fs::write(dest_subdir.join("skip.log"), b"skip").expect("write excluded file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-excluded"),
        OsString::from("--exclude=*.log"),
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.log").exists());
}
