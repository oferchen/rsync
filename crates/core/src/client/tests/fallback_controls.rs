#[cfg(unix)]
#[test]
fn remote_fallback_forwards_partial_dir_argument() {
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
    args.partial = true;
    args.partial_dir = Some(PathBuf::from(".rsync-partial"));
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
    assert!(captured.lines().any(|line| line == "--partial"));
    assert!(captured.lines().any(|line| line == "--partial-dir"));
    assert!(captured.lines().any(|line| line == ".rsync-partial"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delay_updates_flag() {
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
    args.delay_updates = true;
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
    assert!(captured.lines().any(|line| line == "--delay-updates"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_itemize_changes_flag() {
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

    const ITEMIZE_FORMAT: &str = "%i %n%L";

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.itemize_changes = true;
    args.out_format = Some(OsString::from(ITEMIZE_FORMAT));
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
    assert!(captured.lines().any(|line| line == "--itemize-changes"));
    assert!(captured.lines().any(|line| line == "--out-format"));
    assert!(captured.lines().any(|line| line == ITEMIZE_FORMAT));
}

#[cfg(unix)]
#[test]
fn run_client_or_fallback_uses_fallback_for_remote_operands() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("remote::module"), OsString::from("/tmp/dst")])
        .build();

    let mut args = baseline_fallback_args();
    args.remainder = vec![OsString::from("remote::module"), OsString::from("/tmp/dst")];
    args.fallback_binary = Some(script.into_os_string());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);

    let outcome =
        run_client_or_fallback(config, None, Some(context)).expect("fallback invocation succeeds");

    match outcome {
        ClientOutcome::Fallback(summary) => {
            assert_eq!(summary.exit_code(), 42);
        }
        ClientOutcome::Local(_) => panic!("expected fallback outcome"),
    }

    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "fallback stdout\n"
    );
    assert_eq!(
        String::from_utf8(stderr).expect("stderr utf8"),
        "fallback stderr\n"
    );
}

#[cfg(unix)]
#[test]
fn run_client_or_fallback_handles_delta_mode_locally() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let source_path = temp.path().join("source.txt");
    let dest_path = temp.path().join("dest.txt");
    fs::write(&source_path, b"delta-test").expect("source created");

    let source = OsString::from(source_path.as_os_str());
    let dest = OsString::from(dest_path.as_os_str());

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), dest.clone()])
        .whole_file(false)
        .build();

    let mut args = baseline_fallback_args();
    args.remainder = vec![source, dest];
    args.whole_file = Some(false);
    args.fallback_binary = Some(script.into_os_string());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);

    let outcome =
        run_client_or_fallback(config, None, Some(context)).expect("local delta copy succeeds");

    match outcome {
        ClientOutcome::Local(summary) => {
            assert_eq!(summary.files_copied(), 1);
        }
        ClientOutcome::Fallback(_) => panic!("unexpected fallback execution"),
    }

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(fs::read(dest_path).expect("dest contents"), b"delta-test");
}

#[test]
fn remote_fallback_reports_launch_errors() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let missing = temp.path().join("missing-rsync");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(missing.into_os_string());

    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("spawn failure reported");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains("is not available on PATH"));
}

#[test]
fn remote_fallback_detects_missing_default_binary_on_path() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let _path_guard = EnvGuard::set("PATH", "");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let args = baseline_fallback_args();
    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("missing default binary should surface");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains("is not available on PATH"));
}

#[test]
fn remote_fallback_rejects_recursive_resolution() {
    let _lock = env_lock().lock().expect("env mutex poisoned");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(std::env::current_exe().expect("current exe").into_os_string());

    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("recursive fallback should be rejected");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains("fallback resolution points to this oc-rsync executable"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_reports_stdout_forward_errors() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let mut stdout = FailingWriter;
    let mut stderr = Vec::new();

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script.into_os_string());

    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("stdout forwarding failure surfaces");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains("failed to forward fallback stdout"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_propagates_signal_exit_code() {
    use libc::SIGTERM;

    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_signal_script(temp.path());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script.into_os_string());

    let exit = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(exit, 128 + SIGTERM);
}

#[test]
fn remote_fallback_reports_disabled_override() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set_os(CLIENT_FALLBACK_ENV, OsStr::new("no"));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let args = baseline_fallback_args();
    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("disabled override prevents fallback execution");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains(&format!(
        "remote transfers are unavailable because {CLIENT_FALLBACK_ENV} is disabled"
    )));
}

#[test]
fn builder_forces_event_collection() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .force_event_collection(true)
        .build();

    assert!(config.force_event_collection());
    assert!(config.collect_events());
}

#[test]
fn run_client_reports_missing_operands() {
    let config = ClientConfig::builder().build();
    let error = run_client(config).expect_err("missing operands should error");

    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("missing source operands"));
    assert!(
        rendered.contains(&format!("[client={RUST_VERSION}]")),
        "expected missing operands error to include client trailer"
    );
}

#[test]
fn run_client_handles_delta_transfer_mode_locally() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let destination = tmp.path().join("dest.bin");
    fs::write(&source, b"payload").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ])
        .whole_file(false)
        .build();

    let summary = run_client(config).expect("delta mode executes locally");

    assert_eq!(fs::read(&destination).expect("read dest"), b"payload");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), b"payload".len() as u64);
}

#[test]
fn run_client_copies_single_file() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"example").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), b"example");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), b"example".len() as u64);
    assert!(!summary.compression_used());
    assert!(summary.compressed_bytes().is_none());
}

#[test]
fn run_client_with_compress_records_compressed_bytes() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let destination = tmp.path().join("dest.bin");
    let payload = vec![b'Z'; 32 * 1024];
    fs::write(&source, &payload).expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .compress(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), payload);
    assert!(summary.compression_used());
    let compressed = summary
        .compressed_bytes()
        .expect("compressed bytes recorded");
    assert!(compressed > 0);
    assert!(compressed <= summary.bytes_copied());
}

#[test]
fn run_client_skip_compress_disables_compression_for_matching_suffix() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("archive.gz");
    let destination = tmp.path().join("dest.gz");
    let payload = vec![b'X'; 16 * 1024];
    fs::write(&source, &payload).expect("write source");

    let skip = SkipCompressList::parse("gz").expect("parse list");
    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .compress(true)
        .skip_compress(skip)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), payload);
    assert!(!summary.compression_used());
    assert!(summary.compressed_bytes().is_none());
}

#[test]
fn skip_compress_from_env_parses_list() {
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", "gz,zip");
    let list = skip_compress_from_env("RSYNC_SKIP_COMPRESS")
        .expect("parse env list")
        .expect("list present");

    assert!(list.matches_path(Path::new("file.gz")));
    assert!(list.matches_path(Path::new("archive.zip")));
    assert!(!list.matches_path(Path::new("note.txt")));
}

#[test]
fn skip_compress_from_env_absent_returns_none() {
    let _guard = EnvGuard::remove("RSYNC_SKIP_COMPRESS");
    assert!(
        skip_compress_from_env("RSYNC_SKIP_COMPRESS")
            .expect("absent env")
            .is_none()
    );
}

#[test]
fn skip_compress_from_env_reports_invalid_specification() {
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", "[");
    let error = skip_compress_from_env("RSYNC_SKIP_COMPRESS")
        .expect_err("invalid specification should error");
    let rendered = error.to_string();
    assert!(rendered.contains("RSYNC_SKIP_COMPRESS"));
    assert!(rendered.contains("invalid"));
}

#[cfg(unix)]
#[test]
fn skip_compress_from_env_rejects_non_utf8_values() {
    use std::os::unix::ffi::OsStrExt;

    let bytes = OsStr::from_bytes(&[0xFF]);
    let _guard = EnvGuard::set_os("RSYNC_SKIP_COMPRESS", bytes);
    let error =
        skip_compress_from_env("RSYNC_SKIP_COMPRESS").expect_err("non UTF-8 value should error");
    assert!(
        error
            .to_string()
            .contains("RSYNC_SKIP_COMPRESS accepts only UTF-8")
    );
}

#[test]
fn run_client_remove_source_files_deletes_source() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"move me").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .remove_source_files(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(summary.sources_removed(), 1);
    assert!(!source.exists(), "source should be removed after transfer");
    assert_eq!(fs::read(&destination).expect("read dest"), b"move me");
}

#[test]
fn run_client_remove_source_files_preserves_matched_source() {
    use filetime::{FileTime, set_file_times};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    let payload = b"stable";
    fs::write(&source, payload).expect("write source");
    fs::write(&destination, payload).expect("write destination");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");
    set_file_times(&destination, timestamp, timestamp).expect("set dest times");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .remove_source_files(true)
        .times(true)
        .build();

    let summary = run_client(config).expect("transfer succeeds");

    assert_eq!(summary.sources_removed(), 0, "unchanged sources remain");
    assert!(source.exists(), "matched source should not be removed");
    assert_eq!(fs::read(&destination).expect("read dest"), payload);
}

#[test]
fn run_client_dry_run_skips_copy() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"dry-run").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .dry_run(true)
        .build();

    let summary = run_client(config).expect("dry-run succeeds");

    assert!(!destination.exists());
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn run_client_delete_removes_extraneous_entries() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    {
        use filetime::{FileTime, set_file_times};

        let newer = FileTime::from_unix_time(1_700_000_100, 0);
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let source_path = source_root.join("keep.txt");
        let dest_path = dest_root.join("keep.txt");
        set_file_times(&source_path, newer, newer).expect("set source mtime");
        set_file_times(&dest_path, older, older).expect("set dest mtime");
    }

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_operand, dest_root.clone().into_os_string()])
        .delete(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"fresh"
    );
    assert!(!dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

