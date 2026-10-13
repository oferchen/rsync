/// A daemon pull must deliver byte-identical files with SEND_ZC compiled
/// into the daemon, both by default (runtime off, the plain `TcpStream`
/// writer on every kernel) and from a daemon started with `--zero-copy`
/// (the SEND_ZC writer where the kernel can run it).
///
/// A 4 MiB file of non-repeating bytes spans many 64 KiB frames, so a writer
/// that reordered or recycled a buffer shows up as a content mismatch.
///
/// The session runs in a forked child, so this test cannot read the child's
/// `fast_io::send_zc_stats` counters; the in-process proofs are
/// `daemon_started_without_zero_copy_never_dispatches_send_zc` and
/// `daemon_started_with_zero_copy_dispatches_send_zc`.
#[cfg(unix)]
#[test]
fn daemon_pull_default_send_zc_byte_identical() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

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

    // A root daemon drops to `nobody` by default, which cannot read the 0700
    // tempdir, so a root run serves as root. A non-root daemon cannot setgid,
    // so the lines are omitted there.
    let owner = fs::metadata(temp.path()).expect("tempdir metadata");
    let ids = if std::os::unix::fs::MetadataExt::uid(&owner) == 0 {
        "uid = 0\ngid = 0\n"
    } else {
        ""
    };
    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[mod]\npath = {}\nread only = true\nuse chroot = false\n{ids}",
            module_dir.display(),
        ),
    )
    .expect("write daemon config");

    for (daemon_flags, label) in [(&[][..], "default"), (&["--zero-copy"][..], "--zero-copy")] {
        let dest_dir = dest_dir.join(label);
        fs::create_dir(&dest_dir).expect("create dest");

        let (port, held_listener) = allocate_test_port();
        let mut arguments = vec![
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ];
        arguments.extend(daemon_flags.iter().map(OsString::from));
        let daemon_config = DaemonConfig::builder()
            .disable_default_paths()
            .arguments(arguments)
            .build();
        let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
        drop(probe_stream);

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([
                OsString::from(format!("rsync://127.0.0.1:{port}/mod/")),
                OsString::from(dest_dir.as_os_str()),
            ])
            .build();
        if let Err(e) = core::client::run_client(client_config) {
            let _ = finish_daemon(daemon_handle);
            panic!("{label} daemon pull failed: {e}");
        }

        assert!(
            fs::read(dest_dir.join("large.bin")).expect("read large") == large,
            "large.bin differs after a {label} daemon pull"
        );
        assert_eq!(
            fs::read(dest_dir.join("small.txt")).expect("read small"),
            small,
            "small.txt differs after a {label} daemon pull"
        );

        let _ = finish_daemon(daemon_handle);
    }
}
