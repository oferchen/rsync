#[test]
fn builder_sets_numeric_ids() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .numeric_ids(true)
        .build();

    assert!(config.numeric_ids());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.numeric_ids());
}

#[test]
fn builder_preserves_owner_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .owner(true)
        .build();

    assert!(config.preserve_owner());
    assert!(!config.preserve_group());
}

#[test]
fn builder_preserves_group_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .group(true)
        .build();

    assert!(config.preserve_group());
    assert!(!config.preserve_owner());
}

#[test]
fn builder_preserves_permissions_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .permissions(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(!config.preserve_times());
}

#[test]
fn builder_preserves_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .times(true)
        .build();

    assert!(config.preserve_times());
    assert!(!config.preserve_permissions());
}

#[test]
fn builder_sets_chmod_modifiers() {
    let modifiers = ChmodModifiers::parse("a+rw").expect("chmod parses");
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .chmod(Some(modifiers.clone()))
        .build();

    assert_eq!(config.chmod(), Some(&modifiers));
}

#[test]
fn builder_preserves_devices_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .devices(true)
        .build();

    assert!(config.preserve_devices());
    assert!(!config.preserve_specials());
}

#[test]
fn builder_preserves_specials_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .specials(true)
        .build();

    assert!(config.preserve_specials());
    assert!(!config.preserve_devices());
}

#[test]
fn builder_preserves_hard_links_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .hard_links(true)
        .build();

    assert!(config.preserve_hard_links());
    assert!(!ClientConfig::default().preserve_hard_links());
}

#[cfg(feature = "acl")]
#[test]
fn builder_preserves_acls_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .acls(true)
        .build();

    assert!(config.preserve_acls());
    assert!(!ClientConfig::default().preserve_acls());
}

#[cfg(feature = "xattr")]
#[test]
fn builder_preserves_xattrs_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .xattrs(true)
        .build();

    assert!(config.preserve_xattrs());
    assert!(!ClientConfig::default().preserve_xattrs());
}

#[test]
fn builder_preserves_remove_source_files_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .remove_source_files(true)
        .build();

    assert!(config.remove_source_files());
    assert!(!ClientConfig::default().remove_source_files());
}

#[test]
fn builder_controls_omit_dir_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .omit_dir_times(true)
        .build();

    assert!(config.omit_dir_times());
    assert!(!ClientConfig::default().omit_dir_times());
}

#[test]
fn builder_controls_omit_link_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .omit_link_times(true)
        .build();

    assert!(config.omit_link_times());
    assert!(!ClientConfig::default().omit_link_times());
}

#[test]
fn builder_enables_sparse() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .sparse(true)
        .build();

    assert!(config.sparse());
}

#[test]
fn builder_enables_size_only() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .size_only(true)
        .build();

    assert!(config.size_only());
    assert!(!ClientConfig::default().size_only());
}

#[test]
fn builder_configures_implied_dirs_flag() {
    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(default_config.implied_dirs());
    assert!(ClientConfig::default().implied_dirs());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .implied_dirs(false)
        .build();

    assert!(!disabled.implied_dirs());

    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .implied_dirs(true)
        .build();

    assert!(enabled.implied_dirs());
}

#[test]
fn builder_sets_mkpath_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .mkpath(true)
        .build();

    assert!(config.mkpath());
    assert!(!ClientConfig::default().mkpath());
}

#[test]
fn builder_sets_inplace() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .inplace(true)
        .build();

    assert!(config.inplace());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.inplace());
}

#[test]
fn builder_sets_delay_updates() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delay_updates(true)
        .build();

    assert!(config.delay_updates());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.delay_updates());
}

#[test]
fn builder_sets_copy_dirlinks() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .copy_dirlinks(true)
        .build();

    assert!(enabled.copy_dirlinks());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.copy_dirlinks());
}

#[test]
fn builder_sets_copy_unsafe_links() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .copy_unsafe_links(true)
        .build();

    assert!(enabled.copy_unsafe_links());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.copy_unsafe_links());
}

#[test]
fn builder_sets_keep_dirlinks() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .keep_dirlinks(true)
        .build();

    assert!(enabled.keep_dirlinks());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.keep_dirlinks());
}

#[test]
fn builder_enables_stats() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .stats(true)
        .build();

    assert!(enabled.stats());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.stats());
}

#[cfg(unix)]
#[test]
fn remote_fallback_invocation_forwards_streams() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut args = baseline_fallback_args();
    args.files_from_used = true;
    args.file_list_entries = vec![OsString::from("alpha"), OsString::from("beta")];
    args.fallback_binary = Some(script.into_os_string());

    let exit_code = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert_eq!(exit_code, 42);
    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "alpha\nbeta\nfallback stdout\n"
    );
    assert_eq!(
        String::from_utf8(stderr).expect("stderr utf8"),
        "fallback stderr\n"
    );
}

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

#[test]
fn map_local_copy_error_reports_delete_limit() {
    let mapped = map_local_copy_error(LocalCopyError::delete_limit_exceeded(2));
    assert_eq!(mapped.exit_code(), MAX_DELETE_EXIT_CODE);
    let rendered = mapped.message().to_string();
    assert!(
        rendered.contains("Deletions stopped due to --max-delete limit (2 entries skipped)"),
        "unexpected diagnostic: {rendered}"
    );
}

#[test]
fn write_daemon_password_appends_newline_and_zeroizes_buffer() {
    let mut output = Vec::new();
    let mut secret = b"swordfish".to_vec();

    write_daemon_password(&mut output, &mut secret).expect("write succeeds");

    assert_eq!(output, b"swordfish\n");
    assert!(secret.iter().all(|&byte| byte == 0));
}

#[test]
fn write_daemon_password_handles_existing_newline() {
    let mut output = Vec::new();
    let mut secret = b"hunter2\n".to_vec();

    write_daemon_password(&mut output, &mut secret).expect("write succeeds");

    assert_eq!(output, b"hunter2\n");
    assert!(secret.iter().all(|&byte| byte == 0));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_copy_links_toggle() {
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
    args.copy_links = Some(true);
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
    assert!(captured.lines().any(|line| line == "--copy-links"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.copy_links = Some(false);
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
    assert!(captured.lines().any(|line| line == "--no-copy-links"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_copy_unsafe_links_toggle() {
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
    args.copy_unsafe_links = Some(true);
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
    assert!(captured.lines().any(|line| line == "--copy-unsafe-links"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.copy_unsafe_links = Some(false);
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
    assert!(
        captured
            .lines()
            .any(|line| line == "--no-copy-unsafe-links")
    );
}

