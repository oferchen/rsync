/// End-to-end test for compressed push transfer over daemon protocol.
///
/// Verifies that `--compress` (`-z`) works correctly when pushing files
/// via `rsync://` daemon connections. The test creates source files with
/// highly compressible content (repeated text patterns) and a subdirectory
/// structure, then pushes them to the daemon with compression enabled.
///
/// After the push, the destination must contain all files with correct
/// content, and the transfer summary confirms that files were copied.
///
/// # Upstream Reference
///
/// - `options.c` - compression negotiation and `-z` flag handling
/// - `token.c` - compressed token transmission during transfer
#[cfg(unix)]
#[test]
fn daemon_compress_push_transfers_files_with_compression() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    // Highly compressible content - repeated text patterns that benefit
    // from zlib/zstd compression during wire transfer.
    let compressible_a: Vec<u8> = b"AAAA compress this repeated pattern.\n"
        .iter()
        .copied()
        .cycle()
        .take(8192)
        .collect();

    let compressible_b: Vec<u8> = b"BBBB another repeated line for compression test.\n"
        .iter()
        .copied()
        .cycle()
        .take(6144)
        .collect();

    fs::write(source_dir.join("alpha.txt"), &compressible_a).expect("write alpha.txt");
    fs::write(source_dir.join("beta.txt"), &compressible_b).expect("write beta.txt");

    // Subdirectory with an additional file to verify directory creation
    // works under compressed transfer.
    let sub_dir = source_dir.join("subdir");
    fs::create_dir(&sub_dir).expect("create subdir");

    let compressible_c: Vec<u8> = b"CCCC nested file with compressible data.\n"
        .iter()
        .copied()
        .cycle()
        .take(4096)
        .collect();
    fs::write(sub_dir.join("gamma.txt"), &compressible_c).expect("write gamma.txt");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config with max-sessions=2 (probe + push) ---
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

    // --- Push with compression enabled ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .compress(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 3,
                "compressed push must copy at least 3 files, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("compressed push failed: {e}");
        }
    }

    // --- Verify all files transferred correctly ---
    assert_eq!(
        fs::read(dest_dir.join("alpha.txt")).expect("read dest alpha.txt"),
        compressible_a,
        "alpha.txt content mismatch after compressed push"
    );

    assert_eq!(
        fs::read(dest_dir.join("beta.txt")).expect("read dest beta.txt"),
        compressible_b,
        "beta.txt content mismatch after compressed push"
    );

    // Verify subdirectory was created and nested file transferred
    let dest_gamma = dest_dir.join("subdir").join("gamma.txt");
    assert!(
        dest_gamma.exists(),
        "subdir/gamma.txt must exist at destination after compressed push"
    );
    assert_eq!(
        fs::read(&dest_gamma).expect("read dest gamma.txt"),
        compressible_c,
        "subdir/gamma.txt content mismatch after compressed push"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
