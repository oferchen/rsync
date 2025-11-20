use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn transfer_request_with_executability_preserves_execute_bits() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-exec.txt");
    let destination = tmp.path().join("dest-exec.txt");

    std::fs::write(&source, b"payload").expect("write source");
    std::fs::set_permissions(&source, PermissionsExt::from_mode(0o511)).expect("set source perms");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--executability"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(mode & 0o111, 0o511 & 0o111);
    assert_ne!(mode & 0o666, 0o511 & 0o666);
}
