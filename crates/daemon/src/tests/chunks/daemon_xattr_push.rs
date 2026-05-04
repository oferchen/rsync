/// End-to-end test for extended attribute (xattr) preservation during a daemon
/// push transfer with `-X` (`--xattrs`).
///
/// Verifies that `user.*` namespace xattrs set on source files survive the full
/// pipeline: sender file-list encoding, wire transfer, receiver xattr
/// application, and final disk commit.
///
/// # Scenario
///
/// Source (client side):
///   alpha.txt  - regular file with `user.test_key` = `"test_value"`
///   beta.txt   - regular file with `user.author` = `"oc-rsync"` and
///                `user.version` = `"1"`
///   subdir/    - directory
///     nested.txt - file with `user.nested_attr` = `"deep_value"`
///
/// Destination (daemon module, initially empty):
///   After push with `-X`, every file must carry the same xattrs as the source.
///
/// # Notes
///
/// The `user.*` namespace is writable without root on most Unix filesystems.
/// Some CI environments or filesystem types (e.g., tmpfs on older kernels) may
/// not support xattrs. The test probes for support before proceeding and skips
/// gracefully when unsupported.
///
/// # Upstream Reference
///
/// - `xattrs.c:send_xattr()` - sender-side xattr encoding
/// - `xattrs.c:receive_xattr()` - receiver-side xattr application
/// - `options.c` - `-X` sets `preserve_xattrs`
#[cfg(unix)]
#[test]
fn daemon_xattr_push_preserves_extended_attributes() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let probe_file = temp.path().join(".xattr_probe");
    fs::write(&probe_file, b"probe").expect("write probe file");
    if xattr::set(&probe_file, "user.probe", b"1").is_err() {
        // Filesystem does not support user xattrs - skip gracefully
        eprintln!("skipping daemon_xattr_push: filesystem does not support user xattrs");
        return;
    }
    fs::remove_file(&probe_file).expect("remove probe file");

    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    // alpha.txt - single xattr
    let alpha_path = source_dir.join("alpha.txt");
    fs::write(&alpha_path, b"alpha content\n").expect("write alpha.txt");
    xattr::set(&alpha_path, "user.test_key", b"test_value").expect("set xattr on alpha.txt");

    // beta.txt - multiple xattrs
    let beta_path = source_dir.join("beta.txt");
    fs::write(&beta_path, b"beta content\n").expect("write beta.txt");
    xattr::set(&beta_path, "user.author", b"oc-rsync").expect("set user.author on beta.txt");
    xattr::set(&beta_path, "user.version", b"1").expect("set user.version on beta.txt");

    // subdir/nested.txt - xattr on a file inside a subdirectory
    let subdir = source_dir.join("subdir");
    fs::create_dir(&subdir).expect("create subdir");
    let nested_path = subdir.join("nested.txt");
    fs::write(&nested_path, b"nested content\n").expect("write nested.txt");
    xattr::set(&nested_path, "user.nested_attr", b"deep_value")
        .expect("set xattr on nested.txt");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[pushmod]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n",
        dest_dir.display()
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
            OsString::from("2"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .xattrs(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 3,
                "expected at least 3 files transferred, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("xattr push failed: {e}");
        }
    }

    let dest_alpha = dest_dir.join("alpha.txt");
    let dest_beta = dest_dir.join("beta.txt");
    let dest_nested = dest_dir.join("subdir").join("nested.txt");

    assert!(dest_alpha.exists(), "alpha.txt must exist at destination");
    assert!(dest_beta.exists(), "beta.txt must exist at destination");
    assert!(
        dest_nested.exists(),
        "subdir/nested.txt must exist at destination"
    );

    assert_eq!(
        fs::read(&dest_alpha).expect("read dest alpha"),
        b"alpha content\n"
    );
    assert_eq!(
        fs::read(&dest_beta).expect("read dest beta"),
        b"beta content\n"
    );
    assert_eq!(
        fs::read(&dest_nested).expect("read dest nested"),
        b"nested content\n"
    );


    // alpha.txt: user.test_key = "test_value"
    let alpha_xattr = xattr::get(&dest_alpha, "user.test_key")
        .expect("read user.test_key from dest alpha.txt")
        .expect("user.test_key must exist on dest alpha.txt");
    assert_eq!(
        alpha_xattr,
        b"test_value",
        "user.test_key value mismatch on alpha.txt"
    );

    // beta.txt: user.author = "oc-rsync", user.version = "1"
    let beta_author = xattr::get(&dest_beta, "user.author")
        .expect("read user.author from dest beta.txt")
        .expect("user.author must exist on dest beta.txt");
    assert_eq!(
        beta_author,
        b"oc-rsync",
        "user.author value mismatch on beta.txt"
    );

    let beta_version = xattr::get(&dest_beta, "user.version")
        .expect("read user.version from dest beta.txt")
        .expect("user.version must exist on dest beta.txt");
    assert_eq!(
        beta_version, b"1",
        "user.version value mismatch on beta.txt"
    );

    // subdir/nested.txt: user.nested_attr = "deep_value"
    let nested_xattr = xattr::get(&dest_nested, "user.nested_attr")
        .expect("read user.nested_attr from dest nested.txt")
        .expect("user.nested_attr must exist on dest nested.txt");
    assert_eq!(
        nested_xattr,
        b"deep_value",
        "user.nested_attr value mismatch on nested.txt"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
