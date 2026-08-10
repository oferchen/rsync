use super::common::*;
use super::*;

#[test]
fn transfer_request_with_relative_preserves_parent_directories() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("src");
    let destination_root = tmp.path().join("dest");
    std::fs::create_dir_all(source_root.join("foo/bar")).expect("create source tree");
    std::fs::create_dir_all(&destination_root).expect("create destination");
    let source_file = source_root.join("foo").join("bar").join("relative.txt");
    std::fs::write(&source_file, b"relative").expect("write source");

    let operand = source_root
        .join(".")
        .join("foo")
        .join("bar")
        .join("relative.txt");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--relative"),
        operand.into_os_string(),
        destination_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let copied = destination_root
        .join("foo")
        .join("bar")
        .join("relative.txt");
    assert_eq!(
        std::fs::read(copied).expect("read copied file"),
        b"relative"
    );
}
