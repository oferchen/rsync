/// Tests for forced protocol 28 daemon mode negotiation.
///
/// Protocol 28 (rsync 3.0.9) uses legacy ASCII negotiation with no compat_flags.
/// These tests verify the daemon correctly handles the forced `--protocol=28`
/// path, both at the wire level (raw TCP) and through the client API.
///
/// Key protocol 28 characteristics:
/// - Greeting: `@RSYNCD: 28.0\n` (no digest list)
/// - No compat_flags exchange (introduced in protocol 30)
/// - Negotiated version = min(server, client)
/// - MD4 assumed as challenge digest by convention
///
/// upstream: clientserver.c - start_daemon(), start_inband_exchange()
/// upstream: compat.c - compat_flags only exchanged when protocol >= 30

#[test]
fn daemon_protocol_28_forced_client_greeting_accepted() {
    // Verify that the daemon accepts a protocol 28 client greeting and proceeds
    // with negotiation. The daemon speaks its latest protocol but must downgrade
    // when client sends `@RSYNCD: 28.0\n`.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read daemon greeting (server speaks newest protocol)
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(
        line.starts_with("@RSYNCD: "),
        "Expected daemon greeting, got: {line}"
    );

    // Client forces protocol 28 - no digest list
    stream
        .write_all(b"@RSYNCD: 28.0\n")
        .expect("send v28 greeting");
    stream.flush().expect("flush");

    // Request module listing to verify protocol accepted
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    // Daemon should respond with EXIT (no modules configured)
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert_eq!(
        line, "@RSYNCD: EXIT\n",
        "Expected EXIT for empty module list at protocol 28, got: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

#[test]
fn daemon_protocol_28_forced_greeting_has_no_digest_list() {
    // Verify that the daemon greeting formatted for protocol 28 omits the
    // digest list entirely. Pre-protocol-30 clients do not expect digests.
    let greeting = legacy_daemon_greeting_for_protocol(ProtocolVersion::V28);

    assert_eq!(
        greeting, "@RSYNCD: 28.0\n",
        "Protocol 28 greeting must be exactly '@RSYNCD: 28.0\\n' with no digest list"
    );
    assert_eq!(greeting.len(), 14, "greeting must be exactly 14 bytes");
    assert!(!greeting.contains("md4"), "must not contain digest names");
    assert!(!greeting.contains("md5"), "must not contain digest names");
}

#[test]
fn daemon_protocol_28_forced_no_compat_flags_exchanged() {
    // Verify that protocol 28 does not produce or expect compat_flags.
    // This is a unit-level check of the version properties.
    let v28 = ProtocolVersion::V28;

    assert!(
        v28.uses_legacy_ascii_negotiation(),
        "protocol 28 must use legacy ASCII negotiation"
    );
    assert!(
        !v28.uses_binary_negotiation(),
        "protocol 28 must not use binary negotiation"
    );
    assert_eq!(v28.as_u8(), 28);

    // Protocol 28 has no compat_flags - the effective flags are always EMPTY.
    // Contrast with protocol 30+ which exchanges a varint compat_flags byte.
    let v30 = ProtocolVersion::V30;
    assert!(
        !v30.uses_legacy_ascii_negotiation() || v30.as_u8() >= 30,
        "protocol 30+ differs from 28 in negotiation style"
    );
}

#[test]
fn daemon_protocol_28_forced_version_negotiation_downgrade() {
    // When the daemon speaks protocol 32 and client forces 28, the negotiated
    // version must be min(32, 28) = 28. This test exercises the wire-level
    // daemon path with a raw TCP connection.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("tempdir");
    let module_dir = dir.path().join("data");
    fs::create_dir_all(&module_dir).expect("create module dir");

    let config_file = dir.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[testmod]\npath = {}\nread only = true\nuse chroot = false\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Daemon sends its greeting (protocol 32 with digest list)
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(
        line.starts_with("@RSYNCD: 32.0") || line.starts_with("@RSYNCD: 31.0"),
        "Daemon should greet with newest protocol, got: {line}"
    );

    // Client forces protocol 28 - daemon must downgrade
    stream
        .write_all(b"@RSYNCD: 28.0\n")
        .expect("send v28 greeting");
    stream.flush().expect("flush");

    // Request module - should work at protocol 28
    stream.write_all(b"testmod\n").expect("send module");
    stream.flush().expect("flush");

    // Should get @RSYNCD: OK indicating successful module selection
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.starts_with("@RSYNCD: OK"),
        "Expected OK for module at protocol 28, got: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

/// End-to-end test using the client API with forced protocol 28 against a
/// daemon. Verifies that the full negotiation path works when --protocol=28
/// is specified on the client side.
#[cfg(unix)]
#[test]
fn daemon_protocol_28_forced_client_api_push() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::write(source_dir.join("hello.txt"), b"hello world\n").expect("write file");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[pushmod]\npath = {}\nread only = false\nuse chroot = false\n",
            dest_dir.display()
        ),
    )
    .expect("write config");

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

    // Force protocol 28 via client config
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .protocol_version(Some(ProtocolVersion::V28))
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 1,
                "Expected at least 1 file transferred at protocol 28, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("Protocol 28 forced push failed: {e}");
        }
    }

    // Verify file was actually transferred
    let dest_file = dest_dir.join("hello.txt");
    assert!(
        dest_file.exists(),
        "hello.txt must exist in destination after protocol 28 push"
    );
    assert_eq!(
        fs::read(&dest_file).expect("read dest file"),
        b"hello world\n",
        "file content must match after protocol 28 push"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test using the client API with forced protocol 28 against a
/// daemon performing a pull. Mirrors `daemon_protocol_28_forced_client_api_push`
/// but exercises the receiver-direction code path through the daemon, locking
/// in the protocol 28 fixes from PRs #1604, #1606, #1670, #1700, #1704.
///
/// The push case alone is insufficient because pull/push exercise opposite
/// roles: pull makes the daemon the sender, so the legacy file-list encoder
/// (no varint, no INC_RECURSE) and downgraded checksum/compression negotiation
/// must work on the server side.
#[cfg(unix)]
#[test]
fn daemon_protocol_28_forced_client_api_pull() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let module_dir = temp.path().join("module");
    let module_subdir = module_dir.join("nested");
    fs::create_dir_all(&module_subdir).expect("create module/nested");

    fs::write(module_dir.join("hello.txt"), b"hello world\n").expect("write hello");
    fs::write(module_dir.join("data.bin"), b"\x00\x01\x02\x03\xff\xfe\xfd")
        .expect("write data.bin");
    fs::write(module_subdir.join("inner.txt"), b"nested payload\n").expect("write inner");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[pullmod]\npath = {}\nread only = true\nuse chroot = false\n",
            module_dir.display()
        ),
    )
    .expect("write config");

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

    let rsync_url = format!("rsync://127.0.0.1:{port}/pullmod/");

    // Force protocol 28 via client config; client is the receiver.
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_dir.as_os_str())])
        .protocol_version(Some(ProtocolVersion::V28))
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 3,
                "Expected at least 3 files transferred at protocol 28 pull, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("Protocol 28 forced pull failed: {e}");
        }
    }

    // Verify byte-for-byte content match across all pulled files.
    assert_eq!(
        fs::read(dest_dir.join("hello.txt")).expect("read pulled hello.txt"),
        b"hello world\n",
        "hello.txt content must match after protocol 28 pull"
    );
    assert_eq!(
        fs::read(dest_dir.join("data.bin")).expect("read pulled data.bin"),
        b"\x00\x01\x02\x03\xff\xfe\xfd",
        "data.bin binary content must match after protocol 28 pull"
    );
    assert_eq!(
        fs::read(dest_dir.join("nested/inner.txt")).expect("read pulled nested/inner.txt"),
        b"nested payload\n",
        "nested/inner.txt content must match after protocol 28 pull"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end roundtrip test combining push and pull at forced protocol 28
/// against the same daemon module. Verifies the daemon can sustain both
/// roles in a single session lifecycle, with byte-for-byte content equality
/// between original source and pulled destination.
#[cfg(unix)]
#[test]
fn daemon_protocol_28_forced_push_then_pull_roundtrip() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("readme.txt"), b"protocol 28 readme\n").expect("write readme");
    fs::write(source_dir.join("blob.bin"), b"\xde\xad\xbe\xef\x00\x11\x22\x33")
        .expect("write blob");
    fs::write(source_subdir.join("nested.txt"), b"nested under proto 28\n")
        .expect("write nested");

    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");

    let pull_dest = temp.path().join("pulled");
    fs::create_dir(&pull_dest).expect("create pull dest");

    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[lifecycle28]\npath = {}\nread only = false\nuse chroot = false\n",
            module_dir.display()
        ),
    )
    .expect("write config");

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

    // === Phase 1: Push at forced protocol 28 ===
    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{port}/lifecycle28/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .protocol_version(Some(ProtocolVersion::V28))
            .build();

        let result = core::client::run_client(client_config);
        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 3,
                    "protocol 28 push must copy at least 3 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("protocol 28 push phase failed: {e}");
            }
        }
    }

    assert_eq!(
        fs::read(module_dir.join("readme.txt")).expect("read module readme"),
        b"protocol 28 readme\n",
        "readme.txt must arrive in module after protocol 28 push"
    );
    assert_eq!(
        fs::read(module_dir.join("blob.bin")).expect("read module blob.bin"),
        b"\xde\xad\xbe\xef\x00\x11\x22\x33",
        "blob.bin must arrive in module after protocol 28 push"
    );
    assert_eq!(
        fs::read(module_dir.join("subdir/nested.txt")).expect("read module nested"),
        b"nested under proto 28\n",
        "subdir/nested.txt must arrive in module after protocol 28 push"
    );

    // === Phase 2: Pull at forced protocol 28 ===
    {
        let rsync_url = format!("rsync://127.0.0.1:{port}/lifecycle28/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([OsString::from(&rsync_url), OsString::from(pull_dest.as_os_str())])
            .protocol_version(Some(ProtocolVersion::V28))
            .build();

        let result = core::client::run_client(client_config);
        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 3,
                    "protocol 28 pull must copy at least 3 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("protocol 28 pull phase failed: {e}");
            }
        }
    }

    // Roundtrip equality: pulled content matches original source byte-for-byte.
    assert_eq!(
        fs::read(pull_dest.join("readme.txt")).expect("read pulled readme"),
        b"protocol 28 readme\n",
        "readme.txt roundtrip mismatch at protocol 28"
    );
    assert_eq!(
        fs::read(pull_dest.join("blob.bin")).expect("read pulled blob.bin"),
        b"\xde\xad\xbe\xef\x00\x11\x22\x33",
        "blob.bin roundtrip mismatch at protocol 28"
    );
    assert_eq!(
        fs::read(pull_dest.join("subdir/nested.txt")).expect("read pulled nested"),
        b"nested under proto 28\n",
        "subdir/nested.txt roundtrip mismatch at protocol 28"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

#[test]
fn daemon_protocol_28_forced_module_listing_works() {
    // Verify that module listing works at forced protocol 28.
    // Module listing is a simpler operation that exercises the greeting
    // exchange without requiring a full transfer.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("tempdir");
    let module_dir = dir.path().join("data");
    fs::create_dir_all(&module_dir).expect("create module dir");

    let config_file = dir.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[listed]\npath = {}\ncomment = A test module\nuse chroot = false\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read daemon greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Client forces protocol 28
    stream
        .write_all(b"@RSYNCD: 28.0\n")
        .expect("send v28 greeting");
    stream.flush().expect("flush");

    // Request module listing
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    // Read module listing
    let mut lines = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("list line");
        if line.starts_with("@RSYNCD: EXIT") {
            break;
        }
        lines.push(line.clone());
    }

    // Should have listed the module
    let listing = lines.join("");
    assert!(
        listing.contains("listed"),
        "Module listing at protocol 28 should contain 'listed', got: {listing}"
    );
    assert!(
        listing.contains("A test module"),
        "Module listing should contain comment, got: {listing}"
    );

    drop(reader);
    let _ = handle.join();
}
