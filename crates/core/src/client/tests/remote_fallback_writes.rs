use super::prelude::*;


#[cfg(unix)]
#[test]
fn remote_fallback_writes_password_to_stdin() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("password.txt");
    let script_path = temp.path().join("capture-password.sh");
    let script = capture_password_script();
    fs::write(&script_path, script).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.password_file = Some(PathBuf::from("-"));
    args.daemon_password = Some(b"topsecret".to_vec());
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert_eq!(exit, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let captured = fs::read(&capture_path).expect("captured password");
    assert_eq!(captured, b"topsecret\n");
}

