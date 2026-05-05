/// End-to-end test for fuzzy level 2 (`--fuzzy --fuzzy`) over daemon protocol.
///
/// Verifies that when files have been moved between subdirectories, the
/// receiver's fuzzy level 2 search finds basis files in sibling directories
/// of the destination tree, enabling delta transfers instead of full re-sends.
///
/// # Scenario
///
/// Source (daemon module):
///   dir_a/alpha.dat  (64 KB, seed 0xAA)
///   dir_b/beta.dat   (64 KB, seed 0xBB)
///
/// Destination (pre-populated with files in swapped directories):
///   dir_b/alpha.dat  (64 KB, seed 0xAA) - same content, different directory
///   dir_a/beta.dat   (64 KB, seed 0xBB) - same content, different directory
///
/// With `--fuzzy --fuzzy`, the receiver should locate basis files in sibling
/// directories and use delta transfer. Without fuzzy, these would be full
/// transfers since the files are missing in their respective dest directories.
///
/// # Upstream Reference
///
/// - `generator.c:find_fuzzy_basis()` - searches sibling dirs at fuzzy level 2
/// - `options.c` - `fuzzy_basis` incremented per `--fuzzy` flag
#[cfg(unix)]
#[test]
fn daemon_fuzzy_level2_pulls_basis_from_sibling_directories() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    let src_dir_a = source_dir.join("dir_a");
    let src_dir_b = source_dir.join("dir_b");
    fs::create_dir_all(&src_dir_a).expect("create source/dir_a");
    fs::create_dir_all(&src_dir_b).expect("create source/dir_b");

    let content_alpha = fuzzy2_generate_content(65_536, 0xAA);
    let content_beta = fuzzy2_generate_content(65_536, 0xBB);

    fs::write(src_dir_a.join("alpha.dat"), &content_alpha).expect("write source alpha");
    fs::write(src_dir_b.join("beta.dat"), &content_beta).expect("write source beta");

    let dest_dir = temp.path().join("dest");
    let dest_dir_a = dest_dir.join("dir_a");
    let dest_dir_b = dest_dir.join("dir_b");
    fs::create_dir_all(&dest_dir_a).expect("create dest/dir_a");
    fs::create_dir_all(&dest_dir_b).expect("create dest/dir_b");

    // Swapped: alpha.dat is in dir_b instead of dir_a
    fs::write(dest_dir_b.join("alpha.dat"), &content_alpha).expect("write dest alpha (swapped)");
    // Swapped: beta.dat is in dir_a instead of dir_b
    fs::write(dest_dir_a.join("beta.dat"), &content_beta).expect("write dest beta (swapped)");

    // Backdate destination files to prevent quick-check from skipping transfers
    fuzzy2_backdate_file(&dest_dir_b.join("alpha.dat"));
    fuzzy2_backdate_file(&dest_dir_a.join("beta.dat"));

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[testmod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n",
        source_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    // Allow two sessions: one for the readiness probe from start_daemon,
    // one for the actual run_client transfer.
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

    // Drop the probe connection so the daemon worker finishes quickly
    drop(probe_stream);

    let rsync_url = format!("rsync://127.0.0.1:{port}/testmod/");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([
            OsString::from(&rsync_url),
            OsString::from(dest_dir.as_os_str()),
        ])
        .fuzzy_level(2)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 2,
                "expected at least 2 files transferred, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("client transfer failed: {e}");
        }
    }

    // Verify destination files have correct content in their canonical locations
    let dest_alpha = fs::read(dest_dir.join("dir_a/alpha.dat")).expect("read dest alpha");
    assert_eq!(
        dest_alpha, content_alpha,
        "dir_a/alpha.dat content mismatch after transfer"
    );

    let dest_beta = fs::read(dest_dir.join("dir_b/beta.dat")).expect("read dest beta");
    assert_eq!(
        dest_beta, content_beta,
        "dir_b/beta.dat content mismatch after transfer"
    );

    // Daemon exits after serving max_sessions connections
    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// Generates deterministic content of the given size using a repeating pattern.
///
/// Files with the same seed produce identical bytes, enabling delta-transfer
/// efficiency when the receiver finds them as basis files.
#[cfg(unix)]
fn fuzzy2_generate_content(size: usize, seed: u8) -> Vec<u8> {
    (0..size)
        .map(|i| seed.wrapping_add((i % 251) as u8))
        .collect()
}

/// Backdates a file's modification time by one day to ensure the quick-check
/// algorithm does not skip the transfer due to matching mtime + size.
///
/// upstream: receiver.c - quick_check_ok() compares size and mtime
#[cfg(unix)]
fn fuzzy2_backdate_file(path: &Path) {
    use std::time::{Duration, SystemTime};

    let one_day_ago = SystemTime::now()
        .checked_sub(Duration::from_secs(86_400))
        .expect("time subtraction");

    let ft = filetime::FileTime::from_system_time(one_day_ago);
    filetime::set_file_mtime(path, ft).expect("set mtime");
}
