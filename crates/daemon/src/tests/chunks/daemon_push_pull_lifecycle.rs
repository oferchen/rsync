/// End-to-end tests for the daemon push/pull lifecycle.
///
/// Verifies the complete roundtrip: client pushes files to a daemon module,
/// then a separate client session pulls them back. The pulled content must
/// match the original source byte-for-byte.
///
/// This exercises the full daemon protocol lifecycle for both directions:
/// 1. Daemon starts and listens on an ephemeral port
/// 2. Client connects via rsync:// and pushes files (sender role)
/// 3. Client connects via rsync:// and pulls files back (receiver role)
/// 4. File content and directory structure are verified at each stage
///
/// # Upstream Reference
///
/// - `clientserver.c` - daemon protocol negotiation
/// - `main.c:client_run()` - orchestrates sender/receiver roles
/// - `sender.c` / `receiver.c` - data transfer in each direction

#[cfg(unix)]
#[test]
fn daemon_push_then_pull_roundtrip_preserves_content() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side for push) ---
    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("readme.txt"), b"project readme\n").expect("write readme");
    fs::write(source_dir.join("data.bin"), b"\x00\x01\x02\x03\xff\xfe\xfd")
        .expect("write binary data");
    fs::write(source_subdir.join("nested.txt"), b"nested file content\n")
        .expect("write nested file");

    // --- Module directory (served by daemon, writable) ---
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");

    // --- Pull destination (client side for pull) ---
    let pull_dest = temp.path().join("pulled");
    fs::create_dir(&pull_dest).expect("create pull dest");

    // --- Daemon config: 5 sessions = probe + push + pull + margin ---
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[lifecycle]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("5"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    // === Phase 1: Push files to daemon module ===
    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{port}/lifecycle/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .build();

        let result = core::client::run_client(client_config);

        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 3,
                    "push must copy at least 3 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("push phase failed: {e}");
            }
        }
    }

    // Verify files arrived in module directory
    assert_eq!(
        fs::read(module_dir.join("readme.txt")).expect("read module readme"),
        b"project readme\n",
        "readme.txt content mismatch in module after push"
    );
    assert_eq!(
        fs::read(module_dir.join("data.bin")).expect("read module data.bin"),
        b"\x00\x01\x02\x03\xff\xfe\xfd",
        "data.bin content mismatch in module after push"
    );
    assert_eq!(
        fs::read(module_dir.join("subdir/nested.txt")).expect("read module nested"),
        b"nested file content\n",
        "nested.txt content mismatch in module after push"
    );

    // === Phase 2: Pull files back from daemon module ===
    {
        let rsync_url = format!("rsync://127.0.0.1:{port}/lifecycle/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([OsString::from(&rsync_url), OsString::from(pull_dest.as_os_str())])
            .build();

        let result = core::client::run_client(client_config);

        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 3,
                    "pull must copy at least 3 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("pull phase failed: {e}");
            }
        }
    }

    // === Verify roundtrip: pulled content matches original source ===
    assert_eq!(
        fs::read(pull_dest.join("readme.txt")).expect("read pulled readme"),
        b"project readme\n",
        "readme.txt roundtrip content mismatch"
    );
    assert_eq!(
        fs::read(pull_dest.join("data.bin")).expect("read pulled data.bin"),
        b"\x00\x01\x02\x03\xff\xfe\xfd",
        "data.bin roundtrip content mismatch"
    );
    assert_eq!(
        fs::read(pull_dest.join("subdir/nested.txt")).expect("read pulled nested"),
        b"nested file content\n",
        "nested.txt roundtrip content mismatch"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test verifying that a push to a daemon module followed by a
/// modification and a second push correctly updates the destination.
///
/// This covers the incremental sync lifecycle:
/// 1. Initial push seeds the module
/// 2. Source files are modified
/// 3. Second push updates only the changed files
/// 4. Module reflects the latest source state
///
/// # Upstream Reference
///
/// - `generator.c` - quick-check comparison triggers re-transfer
/// - `receiver.c:receive_data()` - applies updated file data
#[cfg(unix)]
#[test]
fn daemon_push_incremental_update_lifecycle() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    fs::write(source_dir.join("config.txt"), b"version=1\n").expect("write config v1");
    fs::write(source_dir.join("stable.txt"), b"unchanged content\n").expect("write stable");

    // --- Module directory ---
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");

    // --- Daemon config ---
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[incmod]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("4"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    // === Phase 1: Initial push ===
    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{port}/incmod/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .build();

        let result = core::client::run_client(client_config);
        if let Err(e) = &result {
            let _ = daemon_handle.join();
            panic!("initial push failed: {e}");
        }
    }

    assert_eq!(
        fs::read(module_dir.join("config.txt")).expect("read module config v1"),
        b"version=1\n",
        "config.txt v1 mismatch"
    );

    // Backdate destination so quick-check detects the change
    let old_time = filetime::FileTime::from_unix_time(1_000_000, 0);
    filetime::set_file_mtime(module_dir.join("config.txt"), old_time)
        .expect("backdate config.txt");
    filetime::set_file_mtime(module_dir.join("stable.txt"), old_time)
        .expect("backdate stable.txt");

    // === Modify source: update config.txt, add new file ===
    fs::write(source_dir.join("config.txt"), b"version=2\nupdated=true\n")
        .expect("write config v2");
    fs::write(source_dir.join("new_file.txt"), b"brand new\n").expect("write new_file");

    // === Phase 2: Incremental push ===
    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{port}/incmod/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .build();

        let result = core::client::run_client(client_config);
        if let Err(e) = &result {
            let _ = daemon_handle.join();
            panic!("incremental push failed: {e}");
        }
    }

    // Verify updated content
    assert_eq!(
        fs::read(module_dir.join("config.txt")).expect("read module config v2"),
        b"version=2\nupdated=true\n",
        "config.txt must reflect v2 after incremental push"
    );
    assert_eq!(
        fs::read(module_dir.join("stable.txt")).expect("read module stable"),
        b"unchanged content\n",
        "stable.txt must be preserved after incremental push"
    );
    assert_eq!(
        fs::read(module_dir.join("new_file.txt")).expect("read module new_file"),
        b"brand new\n",
        "new_file.txt must appear after incremental push"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test for pull from a daemon module with pre-populated content.
///
/// Verifies that a client can pull a directory tree from a read-only daemon
/// module and that the destination matches the module contents exactly.
///
/// # Upstream Reference
///
/// - `clientserver.c:start_daemon_client()` - client-side daemon connection
/// - `receiver.c` - file reception and commit
#[cfg(unix)]
#[test]
fn daemon_pull_lifecycle_copies_full_tree() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Module directory (pre-populated, read-only) ---
    let module_dir = temp.path().join("module");
    let module_subdir = module_dir.join("docs");
    let module_deep = module_dir.join("docs/api");
    fs::create_dir_all(&module_deep).expect("create module/docs/api");

    fs::write(module_dir.join("index.html"), b"<html>root</html>\n").expect("write index");
    fs::write(module_subdir.join("guide.md"), b"# User Guide\n\nIntro paragraph.\n")
        .expect("write guide");
    fs::write(module_deep.join("reference.json"), b"{\"version\": 1}\n")
        .expect("write reference");
    // Include an empty file to verify zero-length files transfer correctly
    fs::write(module_dir.join("empty.txt"), b"").expect("write empty file");

    // --- Pull destination ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest dir");

    // --- Daemon config (read-only module) ---
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[docs]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("3"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    // --- Pull from daemon ---
    let rsync_url = format!("rsync://127.0.0.1:{port}/docs/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_dir.as_os_str())])
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            // At least the 4 files (index, guide, reference, empty)
            assert!(
                summary.files_copied() >= 4,
                "pull must copy at least 4 files, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("pull failed: {e}");
        }
    }

    // Verify complete tree was pulled
    assert_eq!(
        fs::read(dest_dir.join("index.html")).expect("read pulled index"),
        b"<html>root</html>\n",
        "index.html content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("docs/guide.md")).expect("read pulled guide"),
        b"# User Guide\n\nIntro paragraph.\n",
        "guide.md content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("docs/api/reference.json")).expect("read pulled reference"),
        b"{\"version\": 1}\n",
        "reference.json content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("empty.txt")).expect("read pulled empty"),
        b"",
        "empty.txt must be zero-length"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test for push lifecycle with permission preservation.
///
/// Verifies that file permissions set on source files are preserved when
/// pushing to a daemon module, confirming metadata flows through the
/// daemon protocol correctly.
///
/// # Upstream Reference
///
/// - `rsync.c:set_file_attrs()` - applies preserved metadata
/// - `generator.c` - metadata comparison for incremental updates
#[cfg(unix)]
#[test]
fn daemon_push_lifecycle_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source with specific permissions ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    let script_path = source_dir.join("run.sh");
    fs::write(&script_path, b"#!/bin/sh\necho hello\n").expect("write script");
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
        .expect("chmod script 755");

    let config_path = source_dir.join("settings.conf");
    fs::write(&config_path, b"key=value\n").expect("write settings");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o644))
        .expect("chmod settings 644");

    // --- Module directory ---
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");

    // --- Daemon config ---
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[permmod]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("3"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    // --- Push with permission preservation ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/permmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .permissions(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 2,
                "push must copy at least 2 files, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("permission push failed: {e}");
        }
    }

    // Verify permissions were preserved
    let dest_script = fs::metadata(module_dir.join("run.sh")).expect("stat dest script");
    assert_eq!(
        dest_script.permissions().mode() & 0o777,
        0o755,
        "run.sh should have 755 permissions"
    );

    let dest_config = fs::metadata(module_dir.join("settings.conf")).expect("stat dest config");
    assert_eq!(
        dest_config.permissions().mode() & 0o777,
        0o644,
        "settings.conf should have 644 permissions"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test verifying that pushing to a read-only module is rejected.
///
/// The daemon must refuse write operations when the module is configured
/// with `read only = true` (the default). This tests the access control
/// enforcement during the daemon push lifecycle.
///
/// # Upstream Reference
///
/// - `clientserver.c:rsync_module()` - checks read_only before allowing push
#[cfg(unix)]
#[test]
fn daemon_push_lifecycle_rejects_read_only_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");
    fs::write(source_dir.join("file.txt"), b"should not arrive\n").expect("write file");

    // --- Module directory (read-only) ---
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");

    // --- Daemon config with read only = true ---
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[readonly]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("3"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    // --- Attempt push to read-only module ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/readonly/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .build();

    let result = core::client::run_client(client_config);

    // Push to read-only module must fail
    assert!(
        result.is_err(),
        "push to read-only module should be rejected"
    );

    // Module directory must remain empty
    assert!(
        !module_dir.join("file.txt").exists(),
        "file.txt must not be written to read-only module"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
