/// End-to-end test for `--delete` stats emission during a daemon upload.
///
/// Reproduces the URV-6 upload-direction gap: with the default-features build
/// the receiver runs `run_pipelined_incremental`, which previously skipped
/// `delete_extraneous_files` entirely. Even with the URV-6.a `--stats` gate
/// fix, the receiver never carried a non-zero `DeleteStats` and never wrote
/// `NDX_DEL_STATS` during the goodbye handshake. The client sender therefore
/// surfaced "Number of deleted files: 0" no matter how many extraneous
/// entries existed.
///
/// The test performs an upload with `--delete --stats`. The destination is
/// pre-seeded with an extra file absent from the source. After the upload
/// the destination must be in sync and the client summary must report
/// exactly one deleted entry.
///
/// # Upstream Reference
///
/// - `generator.c:do_delete_pass()` - full tree walk deletion sweep
/// - `generator.c:2393-2398` - `delete_mode || force_delete || read_batch`
///   gate for early `write_del_stats()`
/// - `main.c:225-238` - `write_del_stats()` wire format
#[cfg(unix)]
#[test]
fn daemon_delete_push_reports_delete_stats() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");
    fs::write(source_dir.join("file_a.txt"), "contents of A\n").expect("write a");
    fs::write(source_dir.join("file_b.txt"), "contents of B\n").expect("write b");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    // Seed the destination with the same two files plus an extra entry that
    // is absent from the source set - this is the one `--delete` must remove.
    fs::write(dest_dir.join("file_a.txt"), "contents of A\n").expect("seed a");
    fs::write(dest_dir.join("file_b.txt"), "contents of B\n").expect("seed b");
    fs::write(dest_dir.join("extra.txt"), "should be deleted\n").expect("seed extra");

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
            OsString::from("4"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .delete(true)
        .stats(true)
        .build();

    let result = core::client::run_client(client_config);

    let summary = match result {
        Ok(s) => s,
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("delete-stats push failed: {e}");
        }
    };

    assert!(
        dest_dir.join("file_a.txt").exists(),
        "file_a.txt must survive the upload"
    );
    assert!(
        dest_dir.join("file_b.txt").exists(),
        "file_b.txt must survive the upload"
    );
    assert!(
        !dest_dir.join("extra.txt").exists(),
        "extra.txt must be removed by --delete on the daemon-receive side"
    );

    // The receiver must propagate NDX_DEL_STATS so the sender's --stats output
    // reflects the count. Before URV-6.b + this PR the counter stayed at zero
    // even though the deletion happened on disk.
    assert_eq!(
        summary.items_deleted(),
        1,
        "client summary must report one deleted file (NDX_DEL_STATS not propagated)"
    );

    let _ = daemon_handle.join();
}
