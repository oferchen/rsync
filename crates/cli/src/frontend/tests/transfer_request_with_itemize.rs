use super::common::*;
use super::*;

#[test]
fn transfer_request_with_itemize_changes_renders_itemized_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"itemized").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--itemize-changes"),
        source.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(
        String::from_utf8(stdout).expect("utf8"),
        ">f+++++++++ source.txt\n"
    );

    let destination = dest_dir.join("source.txt");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"itemized"
    );
}

#[test]
fn transfer_request_with_no_itemize_changes_suppresses_itemized_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"itemized").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--itemize-changes"),
        OsString::from("--no-itemize-changes"),
        source.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.is_empty());

    let destination = dest_dir.join("source.txt");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"itemized"
    );
}
