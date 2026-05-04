use super::common::*;
use super::*;

#[test]
fn transfer_request_with_cvs_exclude_skips_default_patterns() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
    let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new(""));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("core"), b"core").expect("write core");
    let git_dir = source_root.join(".git");
    std::fs::create_dir_all(&git_dir).expect("create git dir");
    std::fs::write(git_dir.join("config"), b"git").expect("write git config");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--cvs-exclude"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("core").exists());
    assert!(!copied_root.join(".git").exists());
}

#[test]
fn transfer_request_with_cvs_exclude_respects_cvsignore_files() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
    let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new(""));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.log"), b"skip").expect("write skip");
    std::fs::write(source_root.join(".cvsignore"), b"skip.log\n").expect("write cvsignore");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--cvs-exclude"),
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

#[test]
fn transfer_request_with_cvs_exclude_respects_cvsignore_env() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
    let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new("*.tmp"));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--cvs-exclude"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}
