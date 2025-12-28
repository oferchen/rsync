use super::common::*;
use super::*;

#[test]
fn password_file_requires_daemon_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let password_path = temp.path().join("local.pw");
    std::fs::write(&password_path, b"secret\n").expect("write password");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&password_path)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&password_path, permissions).expect("set permissions");
    }

    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--password-file={}", password_path.display())),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("--password-file"));
    assert!(rendered.contains("rsync daemon"));
    assert!(!destination.exists());
}

#[test]
fn password_file_dash_conflicts_with_files_from_dash() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--files-from=-"),
        OsString::from("--password-file=-"),
        OsString::from("/tmp/dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("--password-file=- cannot be combined with --files-from=-"));
}
