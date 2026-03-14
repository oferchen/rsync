// End-to-end test for `--dry-run` push over daemon protocol across versions.
//
// Verifies that a push transfer with `--dry-run` works correctly at protocol
// versions 31 and 32. For each version, the test confirms that:
// - The transfer completes successfully.
// - Files that would be transferred are reported in the summary.
// - No files are actually written to the destination module.
//
// Protocol 31 and 32 differ in wire format details (varint encoding, checksum
// negotiation) but dry-run semantics must be identical.
//
// Upstream reference:
// - clientserver.c - daemon closes socket early during dry-run push
// - options.c - dry_run sets !do_xfers
// - compat.c:211-227 - protocol version negotiation via --protocol

/// Runs a single dry-run push at a given protocol version and asserts no files
/// are written while the summary reports the expected count.
#[cfg(unix)]
fn run_dry_run_push_at_protocol(protocol: ProtocolVersion) {
    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("file_a.txt"), b"alpha content\n").expect("write file_a");
    fs::write(source_dir.join("file_b.txt"), b"beta content\n").expect("write file_b");
    fs::write(source_subdir.join("file_c.txt"), b"gamma content\n").expect("write file_c");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config ---
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[drymod]\n\
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

    // --- Run client push with --dry-run at specified protocol ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/drymod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .dry_run(true)
        .protocol_version(Some(protocol))
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 3,
                "protocol {}: expected at least 3 files reported, got {}",
                protocol.as_u8(),
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!(
                "dry-run push at protocol {} failed: {e}",
                protocol.as_u8()
            );
        }
    }

    // Verify destination remains empty
    assert!(
        !dest_dir.join("file_a.txt").exists(),
        "protocol {}: file_a.txt must not exist after dry-run push",
        protocol.as_u8()
    );
    assert!(
        !dest_dir.join("file_b.txt").exists(),
        "protocol {}: file_b.txt must not exist after dry-run push",
        protocol.as_u8()
    );
    assert!(
        !dest_dir.join("subdir").exists(),
        "protocol {}: subdir must not exist after dry-run push",
        protocol.as_u8()
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

#[cfg(unix)]
#[test]
fn daemon_dry_run_push_protocol_32() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    run_dry_run_push_at_protocol(ProtocolVersion::V32);
}

#[cfg(unix)]
#[test]
fn daemon_dry_run_push_protocol_31() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    run_dry_run_push_at_protocol(ProtocolVersion::V31);
}
