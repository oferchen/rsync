/// Daemon must serve a real file pulled from a `path = /` module with
/// `use chroot = no` (URV-4.a coverage gap for the upstream rsync 3.4.4 #897
/// scenario).
///
/// PR #5532 added `run_daemon_serves_module_with_root_path_no_chroot` which
/// only exercises `#list` against a `path = /` module. Upstream #897 was
/// specifically about reading a file from such a module - oc-rsync replaces
/// the upstream `secure_relative_open` per-file path check with a
/// module-rooted `ServerConfig` plus canonical-prefix validation, so the
/// upstream bug class does not reproduce here, but the file-pull path was
/// not covered end-to-end. This test closes that gap.
///
/// # Scenario
///
/// - Configure `[root]` with `path = /`, `use chroot = no`, `read only = yes`.
/// - Create a deterministic source file under a tempdir.
/// - Pull it via `rsync://127.0.0.1:port/root<absolute_tempdir>/source.txt`.
/// - Assert the destination file matches the source byte-for-byte.
///
/// # Upstream Reference
///
/// - `loadparm.c` (P_PATH) preserves the bare slash verbatim.
/// - `clientserver.c` chdir()s into the module path then opens the
///   remainder of the request relative to that root when chroot is off.
#[cfg(unix)]
#[test]
fn run_daemon_pulls_file_from_root_path_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("urv4a-source");
    fs::create_dir(&source_dir).expect("create source dir");

    let source_file = source_dir.join("source.txt");
    let source_contents = b"urv-4a deterministic payload\n";
    fs::write(&source_file, source_contents).expect("write source file");

    // Canonicalize so symlinks in the tempdir prefix (e.g. /var -> /private/var
    // on macOS) are resolved before being embedded in the rsync:// URL.
    let canonical_source = source_file
        .canonicalize()
        .expect("canonicalize source file");
    let relative_to_root = canonical_source
        .strip_prefix("/")
        .expect("absolute path under /")
        .to_str()
        .expect("UTF-8 path")
        .to_owned();

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = "[root]\npath = /\nuse chroot = no\nread only = yes\n";
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
            OsString::from("2"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    let dest_file = temp.path().join("dest.txt");
    let rsync_url = format!("rsync://127.0.0.1:{port}/root/{relative_to_root}");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_file.as_os_str())])
        .build();

    let result = core::client::run_client(client_config);

    let summary = match result {
        Ok(summary) => summary,
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("pull from path=/ module failed: {e}");
        }
    };

    assert_eq!(
        summary.files_copied(),
        1,
        "expected exactly one file to be copied, got {}",
        summary.files_copied()
    );

    let pulled = fs::read(&dest_file).expect("read destination file");
    assert_eq!(
        pulled, source_contents,
        "destination file content must match source from path=/ module"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
