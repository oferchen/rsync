/// A default daemon pull must deliver byte-identical files now that the
/// daemon-sender's socket writer dispatches `IORING_OP_SEND_ZC` by default.
///
/// On a Linux 6.0+ kernel the session's multiplexed frames leave through the
/// SEND_ZC pipeline, whose deferred page-release accounting is what could
/// corrupt a payload if it recycled a buffer the kernel still holds. A 4 MiB
/// file of non-repeating bytes spans many 64 KiB frames and buffer recycles,
/// so any such reuse shows up as a content mismatch.
///
/// The session runs in a forked child, so this test cannot read the child's
/// `fast_io::send_zc_stats` counters; the in-process dispatch proof is
/// `daemon_socket_writer_dispatches_send_zc_by_default`. The environment line
/// states whether this run could have exercised SEND_ZC at all.
#[cfg(unix)]
#[test]
fn daemon_pull_default_send_zc_byte_identical() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    eprintln!(
        "[send_zc env] daemon_pull_default_send_zc_byte_identical: {}",
        match (
            fast_io::io_uring::is_io_uring_available(),
            fast_io::io_uring::send_zc_supported(),
        ) {
            (false, _) => "io_uring unavailable - the daemon uses plain sends",
            (true, false) => "SEND_ZC opcode unsupported (kernel < 6.0) - the daemon uses plain sends",
            (true, true) => "ok - the daemon session dispatches IORING_OP_SEND_ZC",
        }
    );

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module");

    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let large: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let small = b"small file\n".to_vec();
    fs::write(module_dir.join("large.bin"), &large).expect("write large");
    fs::write(module_dir.join("small.txt"), &small).expect("write small");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // Serve as the user that owns the fixture, so a root run (which would
    // otherwise drop to `nobody`) can still read the 0700 tempdir.
    let owner = fs::metadata(temp.path()).expect("tempdir metadata");
    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[mod]\npath = {}\nread only = true\nuse chroot = false\nuid = {}\ngid = {}\n",
            module_dir.display(),
            std::os::unix::fs::MetadataExt::uid(&owner),
            std::os::unix::fs::MetadataExt::gid(&owner),
        ),
    )
    .expect("write daemon config");

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

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([
            OsString::from(format!("rsync://127.0.0.1:{port}/mod/")),
            OsString::from(dest_dir.as_os_str()),
        ])
        .build();
    let result = core::client::run_client(client_config);
    if let Err(e) = &result {
        let _ = finish_daemon(daemon_handle);
        panic!("default daemon pull failed: {e}");
    }

    assert!(
        fs::read(dest_dir.join("large.bin")).expect("read large") == large,
        "large.bin differs after a default (SEND_ZC-capable) daemon pull"
    );
    assert_eq!(
        fs::read(dest_dir.join("small.txt")).expect("read small"),
        small,
        "small.txt differs after a default daemon pull"
    );

    let _ = finish_daemon(daemon_handle);
}
