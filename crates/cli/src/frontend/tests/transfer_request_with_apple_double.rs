use super::common::*;
use super::*;

#[test]
fn transfer_request_with_apple_double_skip_excludes_dot_underscore_files() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("._keep.txt"), b"sidecar").expect("write sidecar");
    let nested = source_root.join("nested");
    std::fs::create_dir_all(&nested).expect("create nested");
    std::fs::write(nested.join("data.bin"), b"data").expect("write data");
    std::fs::write(nested.join("._data.bin"), b"sidecar").expect("write nested sidecar");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        OsString::from("--apple-double-skip"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("._keep.txt").exists());
    assert!(copied_root.join("nested/data.bin").exists());
    assert!(!copied_root.join("nested/._data.bin").exists());
}

#[test]
fn transfer_request_without_apple_double_skip_includes_dot_underscore_files() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("._keep.txt"), b"sidecar").expect("write sidecar");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(copied_root.join("._keep.txt").exists());
}
