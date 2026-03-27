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

    // Source: executable with restricted read bits (0o751 = rwxr-x--x)
    std::fs::write(&source, b"payload").expect("write source");
    std::fs::set_permissions(&source, PermissionsExt::from_mode(0o751)).expect("set source perms");

    // Pre-create destination with different permissions (0o620 = rw--w----)
    // so we can verify --executability only toggles execute bits.
    std::fs::write(&destination, b"existing").expect("write dest");
    std::fs::set_permissions(&destination, PermissionsExt::from_mode(0o620))
        .expect("set dest perms");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--executability"),
        OsString::from("--ignore-times"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let mode = metadata.permissions().mode() & 0o777;
    // Execute bits should be set (source has execute bits)
    assert_ne!(mode & 0o111, 0, "execute bits should be preserved from source");
    // Non-execute bits should remain from destination, not source
    assert_ne!(
        mode & 0o666,
        0o751 & 0o666,
        "non-execute bits should not be copied from source"
    );
}
