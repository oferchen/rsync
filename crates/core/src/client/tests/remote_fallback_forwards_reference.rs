use super::prelude::*;


#[cfg(unix)]
#[test]
fn remote_fallback_forwards_reference_directory_flags() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.compare_destinations = vec![OsString::from("compare-one"), OsString::from("compare-two")];
    args.copy_destinations = vec![OsString::from("copy-one")];
    args.link_destinations = vec![OsString::from("link-one"), OsString::from("link-two")];
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    let lines: Vec<&str> = captured.lines().collect();
    let expected_pairs = [
        ("--compare-dest", "compare-one"),
        ("--compare-dest", "compare-two"),
        ("--copy-dest", "copy-one"),
        ("--link-dest", "link-one"),
        ("--link-dest", "link-two"),
    ];

    for (flag, path) in expected_pairs {
        assert!(
            lines
                .windows(2)
                .any(|window| window[0] == flag && window[1] == path),
            "missing pair {flag} {path} in {:?}",
            lines
        );
    }
}

