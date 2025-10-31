use super::common::*;
use super::*;

#[test]
fn transfer_request_with_out_format_renders_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"format").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%f %b"),
        source.clone().into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(String::from_utf8(stdout).expect("utf8"), "source.txt 6\n");

    let destination = dest_dir.join("source.txt");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"format"
    );
}
