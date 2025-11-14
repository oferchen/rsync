#[cfg(unix)]
#[test]
fn remote_fallback_forwards_checksum_choice() {
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
    args.fallback_binary = Some(script_path.into_os_string());
    args.checksum = true;
    args.checksum_choice = Some(OsString::from("xxh128"));
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
    assert!(captured.lines().any(|line| line == "--checksum"));
    assert!(
        captured
            .lines()
            .any(|line| line == "--checksum-choice=xxh128")
    );
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_checksum_seed() {
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
    args.fallback_binary = Some(script_path.into_os_string());
    args.checksum = true;
    args.checksum_seed = Some(123);
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
    assert!(captured.lines().any(|line| line == "--checksum"));
    assert!(captured.lines().any(|line| line == "--checksum-seed=123"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_copy_dirlinks_flag() {
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
    args.copy_dirlinks = true;
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
    assert!(captured.lines().any(|line| line == "--copy-dirlinks"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_copy_devices_flag() {
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
    args.copy_devices = true;
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
    assert!(captured.lines().any(|line| line == "--copy-devices"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_ignore_missing_args_flag() {
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
    args.ignore_missing_args = true;
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
    assert!(captured.lines().any(|line| line == "--ignore-missing-args"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_inc_recursive_toggle() {
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
    args.inc_recursive = Some(true);
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
    assert!(captured.lines().any(|line| line == "--inc-recursive"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.inc_recursive = Some(false);
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
    assert!(captured.lines().any(|line| line == "--no-inc-recursive"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_one_file_system_toggle() {
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
    args.one_file_system = Some(true);
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
    assert!(captured.lines().any(|line| line == "--one-file-system"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.one_file_system = Some(false);
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
    assert!(captured.lines().any(|line| line == "--no-one-file-system"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_backup_arguments() {
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
    args.backup = true;
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
    assert!(captured.lines().any(|line| line == "--backup"));
    assert!(!captured.lines().any(|line| line == "--backup-dir"));
    assert!(!captured.lines().any(|line| line == "--suffix"));

    fs::write(&capture_path, b"").expect("truncate capture");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.backup = true;
    args.backup_dir = Some(PathBuf::from("backups"));
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
    assert!(captured.lines().any(|line| line == "--backup"));
    assert!(captured.lines().any(|line| line == "--backup-dir"));
    assert!(captured.lines().any(|line| line == "backups"));

    fs::write(&capture_path, b"").expect("truncate capture");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.backup = true;
    args.backup_suffix = Some(OsString::from(".bak"));
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
    assert!(captured.lines().any(|line| line == "--backup"));
    assert!(captured.lines().any(|line| line == "--suffix"));
    assert!(captured.lines().any(|line| line == ".bak"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_keep_dirlinks_flags() {
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
    args.keep_dirlinks = Some(true);
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
    assert!(captured.lines().any(|line| line == "--keep-dirlinks"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.keep_dirlinks = Some(false);
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
    assert!(captured.lines().any(|line| line == "--no-keep-dirlinks"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_safe_links_flag() {
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
    args.safe_links = true;
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
    assert!(captured.lines().any(|line| line == "--safe-links"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.safe_links = false;
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
    assert!(!captured.lines().any(|line| line == "--safe-links"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_chmod_arguments() {
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
    args.fallback_binary = Some(script_path.into_os_string());
    args.chmod = vec![OsString::from("Du+rwx"), OsString::from("Fgo-w")];
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
    assert!(captured.lines().any(|line| line == "--chmod=Du+rwx"));
    assert!(captured.lines().any(|line| line == "--chmod=Fgo-w"));
}

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
            "missing pair {flag} {path} in {lines:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_cvs_exclude_flag() {
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
    args.fallback_binary = Some(script_path.into_os_string());
    args.cvs_exclude = true;
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
    assert!(captured.lines().any(|line| line == "--cvs-exclude"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_block_size_argument() {
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
    args.fallback_binary = Some(script_path.into_os_string());
    args.block_size = Some(OsString::from("24576"));
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
    assert!(captured.lines().any(|line| line == "--block-size=24576"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_mkpath_flag() {
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
    args.mkpath = true;
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
    assert!(captured.lines().any(|line| line == "--mkpath"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_sockopts_argument() {
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
    args.fallback_binary = Some(script_path.into_os_string());
    args.sockopts = Some(OsString::from("SO_SNDBUF=4096,SO_RCVBUF=4096"));
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
    assert!(captured
        .lines()
        .any(|line| line == "--sockopts=SO_SNDBUF=4096,SO_RCVBUF=4096"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_blocking_io_flags() {
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
    args.blocking_io = Some(true);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args.clone())
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--blocking-io"));

    let capture_path_no = temp.path().join("args-no-blocking.txt");
    let script_path_no = temp.path().join("capture-no.sh");
    fs::write(&script_path_no, capture_args_script()).expect("script written");
    let metadata_no = fs::metadata(&script_path_no).expect("script metadata");
    let mut permissions_no = metadata_no.permissions();
    permissions_no.set_mode(0o755);
    fs::set_permissions(&script_path_no, permissions_no).expect("script permissions set");

    let mut args_no = args;
    args_no.fallback_binary = Some(script_path_no.into_os_string());
    args_no.blocking_io = Some(false);
    args_no.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path_no.display()
    ))];

    let mut stdout_no = Vec::new();
    let mut stderr_no = Vec::new();
    run_remote_transfer_fallback(&mut stdout_no, &mut stderr_no, args_no)
        .expect("fallback invocation succeeds");

    assert!(stdout_no.is_empty());
    assert!(stderr_no.is_empty());
    let captured_no = fs::read_to_string(&capture_path_no).expect("capture contents");
    assert!(captured_no.lines().any(|line| line == "--no-blocking-io"));
    assert!(!captured_no.lines().any(|line| line == "--blocking-io"));
}

