/// End-to-end test for `--delete` stats emission during a daemon pull.
///
/// Pull-direction regression for URV-6: ensure that wiring the upload-direction
/// delete pass does not break the previously-working pull case. On a pull the
/// local client runs the receiver and performs the sweep itself; the per-type
/// counters must surface via `ClientSummary::items_deleted()` from the
/// receiver's `TransferStats.delete_stats` field.
///
/// # Upstream Reference
///
/// - `generator.c:do_delete_pass()` - full tree walk deletion sweep
/// - `receiver.c:delete_in_dir()` - local sweep on the pull-side receiver
#[cfg(unix)]
#[test]
fn daemon_delete_pull_reports_delete_stats() {
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
    fs::write(dest_dir.join("file_a.txt"), "contents of A\n").expect("seed a");
    fs::write(dest_dir.join("file_b.txt"), "contents of B\n").expect("seed b");
    fs::write(dest_dir.join("extra.txt"), "should be deleted\n").expect("seed extra");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[pullmod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n",
        source_dir.display()
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

    let rsync_url = format!("rsync://127.0.0.1:{port}/pullmod/");
    let mut dest_arg = dest_dir.clone().into_os_string();
    dest_arg.push("/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&rsync_url), dest_arg])
        .delete(true)
        .stats(true)
        .build();

    let result = core::client::run_client(client_config);

    let summary = match result {
        Ok(s) => s,
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("delete-stats pull failed: {e}");
        }
    };

    assert!(
        dest_dir.join("file_a.txt").exists(),
        "file_a.txt must survive the pull"
    );
    assert!(
        dest_dir.join("file_b.txt").exists(),
        "file_b.txt must survive the pull"
    );
    assert!(
        !dest_dir.join("extra.txt").exists(),
        "extra.txt must be removed by --delete on the pull-side receiver"
    );

    assert_eq!(
        summary.items_deleted(),
        1,
        "client summary must report one deleted file from the local receiver sweep"
    );

    let _ = daemon_handle.join();
}
