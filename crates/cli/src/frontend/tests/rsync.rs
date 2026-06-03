use super::common::*;
use super::*;

#[test]
fn rsync_path_silently_ignored_for_local_copies() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    std::fs::write(&source, b"content").expect("write source");

    // upstream: options.c stores --rsync-path but only uses it when spawning
    // a remote shell. Local copies silently ignore the option.
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--rsync-path=/opt/custom/rsync"),
        source.into_os_string(),
        dest.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(dest.exists());
}
