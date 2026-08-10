/// Regression test for UTS-v3 Cluster A: the daemon-as-sender goodbye race.
///
/// Cluster A surfaced 4 upstream testsuite failures (`batch-mode`,
/// `alt-dest`, `daemon-gzip-download`, `daemon-refuse-compress`) all reporting
/// "connection unexpectedly closed (N bytes received so far) [receiver]" at
/// near-identical byte cutoffs. Wire-byte capture against the upstream rsync
/// 3.4.3 client showed the oc-rsync daemon-as-sender writing its final
/// `NDX_DONE` / `MSG_STATS` correctly, then immediately calling
/// `shutdown(SHUT_WR)`. The receiver process saw `FIN` before its own
/// generator had relayed the trailing `MSG_STATS` / `NDX_DONE` pair through
/// the receiver-to-generator pipe, abandoned its writes, and emitted the
/// "connection unexpectedly closed" diagnostic.
///
/// Upstream avoids this via its fork model: the sender child holds the only
/// fd reference and `FIN` is queued only after the kernel reaps the exited
/// process - long after the receiver has flushed its goodbye. The threaded
/// daemon's fix in `process_approved_module` drops the half-close and relies
/// on `SO_LINGER` + a read-until-EOF loop, mirroring upstream's
/// `noop_io_until_death()` for the sender side.
///
/// This test exercises the daemon-as-sender + `--compress` (`-z`) pull
/// pattern that triggers the race under upstream's testsuite: a multi-file
/// tree pulled through `core::client::run_client`, with compression enabled
/// on both ends. A successful run requires the daemon to drain the
/// receiver's goodbye writes before closing the socket; if the half-close
/// regresses, `run_client` will surface an early-EOF error from the receiver
/// path.
///
/// # Upstream Reference
///
/// - `cleanup.c:265 close_all()` - sender-side close after `_exit_cleanup`.
/// - `io.c:943-963 noop_io_until_death()` - receiver-side drain pattern.
/// - `main.c:893-923 read_final_goodbye()` - sender's NDX exchange.
#[cfg(unix)]
#[test]
fn daemon_compress_pull_drains_peer_goodbye_for_uts_v3_cluster_a() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // Source tree: mix sizes that span single and multi-frame MSG_DATA
    // chunks under -z. The 1 MB file forces the engine through the
    // io_uring fast path (threshold 1 MB) which writes through `writev`,
    // matching the wire pattern that triggers the cluster A race.
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module");

    let small: Vec<u8> = b"compressible repeated pattern\n"
        .iter()
        .copied()
        .cycle()
        .take(28)
        .collect();
    let medium: Vec<u8> = b"another compressible repeated pattern line\n"
        .iter()
        .copied()
        .cycle()
        .take(52_802)
        .collect();
    let large: Vec<u8> = b"large file content for compression and chunking test\n"
        .iter()
        .copied()
        .cycle()
        .take(1_088_091)
        .collect();

    fs::write(module_dir.join("small.txt"), &small).expect("write small");
    fs::write(module_dir.join("medium.txt"), &medium).expect("write medium");
    fs::write(module_dir.join("large.txt"), &large).expect("write large");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[mod]\n\
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
            OsString::from("2"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    let rsync_url = format!("rsync://127.0.0.1:{port}/mod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([
            OsString::from(&rsync_url),
            OsString::from(dest_dir.as_os_str()),
        ])
        .compress(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 3,
                "compressed pull must copy 3 files, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!(
                "compressed pull failed - cluster A regression? error: {e}\n\
                 if this is 'connection unexpectedly closed (N bytes received so far)' \
                 the daemon goodbye drain has regressed; see \
                 crates/daemon/src/daemon/sections/module_access/transfer.rs"
            );
        }
    }

    assert_eq!(
        fs::read(dest_dir.join("small.txt")).expect("read small"),
        small,
        "small.txt content mismatch after compressed pull"
    );
    assert_eq!(
        fs::read(dest_dir.join("medium.txt")).expect("read medium"),
        medium,
        "medium.txt content mismatch after compressed pull"
    );
    assert_eq!(
        fs::read(dest_dir.join("large.txt")).expect("read large"),
        large,
        "large.txt content mismatch after compressed pull"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
