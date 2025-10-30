use super::common::*;
use super::*;

#[test]
fn combined_archive_and_verbose_flags_are_supported() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("combo.txt");
    let destination = tmp.path().join("combo.out");
    std::fs::write(&source, b"combo").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(rendered.contains("combo.txt"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"combo"
    );
}
