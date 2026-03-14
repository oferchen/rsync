/// End-to-end test for `--compare-dest` push over daemon protocol.
///
/// Verifies that a push transfer with `--compare-dest` skips files that already
/// exist in the reference directory with identical content, so the destination
/// remains empty for unchanged files.
///
/// # Scenario
///
/// Source (client side):
///   unchanged.txt  (content matches reference)
///   changed.txt    (content differs from reference)
///
/// Reference directory (on daemon, sibling of dest):
///   unchanged.txt  (same content as source)
///   changed.txt    (different content from source)
///
/// Destination (daemon module, initially empty):
///   After push, only changed.txt should appear - unchanged.txt is skipped
///   because the reference already has an identical copy.
///
/// # Upstream Reference
///
/// - `generator.c:recv_generator()` - compare_dest lookup before transfer
/// - `options.c` - `--compare-dest` sets `compare_dest` path list
#[cfg(unix)]
#[test]
fn daemon_compare_dest_push_skips_unchanged_files() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    fs::write(source_dir.join("unchanged.txt"), b"shared content\n").expect("write unchanged");
    fs::write(source_dir.join("changed.txt"), b"new version\n").expect("write changed");

    // --- Reference directory (sibling of dest, has old versions) ---
    let ref_dir = temp.path().join("reference");
    fs::create_dir(&ref_dir).expect("create reference");

    fs::write(ref_dir.join("unchanged.txt"), b"shared content\n").expect("write ref unchanged");
    fs::write(ref_dir.join("changed.txt"), b"old version\n").expect("write ref changed");

    // Backdate reference files so quick-check does not falsely match changed.txt
    let old_time = filetime::FileTime::from_unix_time(1_000_000, 0);
    filetime::set_file_mtime(ref_dir.join("unchanged.txt"), old_time)
        .expect("backdate ref unchanged");
    filetime::set_file_mtime(ref_dir.join("changed.txt"), old_time)
        .expect("backdate ref changed");

    // Match source unchanged.txt mtime to the reference so compare-dest sees it as identical
    filetime::set_file_mtime(source_dir.join("unchanged.txt"), old_time)
        .expect("backdate source unchanged");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config ---
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

    // --- Run client push with --compare-dest ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .compare_destination(&ref_dir)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(_summary) => {}
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("compare-dest client push failed: {e}");
        }
    }

    // changed.txt should appear in the destination because it differs from reference
    let dest_changed = dest_dir.join("changed.txt");
    assert!(
        dest_changed.exists(),
        "changed.txt must exist at destination (differs from reference)"
    );
    assert_eq!(
        fs::read(&dest_changed).expect("read changed.txt"),
        b"new version\n",
        "changed.txt content must match source"
    );

    // unchanged.txt should NOT appear in the destination - compare-dest found it in reference
    let dest_unchanged = dest_dir.join("unchanged.txt");
    assert!(
        !dest_unchanged.exists(),
        "unchanged.txt must not exist at destination (compare-dest should skip it)"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test for `--link-dest` push over daemon protocol.
///
/// Verifies that a push transfer with `--link-dest` creates hard links to files
/// in the reference directory when the content is unchanged, rather than copying
/// the data again.
///
/// # Scenario
///
/// Source (client side):
///   unchanged.txt  (content matches reference)
///   new_file.txt   (not in reference)
///
/// Reference directory (on daemon, sibling of dest):
///   unchanged.txt  (same content as source)
///
/// Destination (daemon module, initially empty):
///   After push, unchanged.txt should be a hard link to the reference copy,
///   and new_file.txt should be a regular copy.
///
/// # Upstream Reference
///
/// - `generator.c:recv_generator()` - link_dest hard-links unchanged files
/// - `receiver.c` - falls back to hard-link when basis matches
#[cfg(unix)]
#[test]
fn daemon_link_dest_push_creates_hardlinks() {
    use std::os::unix::fs::MetadataExt;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    fs::write(source_dir.join("unchanged.txt"), b"shared content\n").expect("write unchanged");
    fs::write(source_dir.join("new_file.txt"), b"brand new\n").expect("write new_file");

    // --- Reference directory (sibling of dest) ---
    let ref_dir = temp.path().join("reference");
    fs::create_dir(&ref_dir).expect("create reference");

    fs::write(ref_dir.join("unchanged.txt"), b"shared content\n").expect("write ref unchanged");

    // Backdate reference so quick-check uses size+mtime comparison
    let old_time = filetime::FileTime::from_unix_time(1_000_000, 0);
    filetime::set_file_mtime(ref_dir.join("unchanged.txt"), old_time)
        .expect("backdate ref unchanged");

    // Match source unchanged.txt mtime to reference
    filetime::set_file_mtime(source_dir.join("unchanged.txt"), old_time)
        .expect("backdate source unchanged");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config ---
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

    // --- Run client push with --link-dest ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .link_destination(&ref_dir)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(_summary) => {}
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("link-dest client push failed: {e}");
        }
    }

    // unchanged.txt should be a hard link to the reference copy
    let dest_unchanged = dest_dir.join("unchanged.txt");
    assert!(
        dest_unchanged.exists(),
        "unchanged.txt must exist at destination"
    );
    assert_eq!(
        fs::read(&dest_unchanged).expect("read unchanged.txt"),
        b"shared content\n",
        "unchanged.txt content mismatch"
    );

    let dest_meta = fs::metadata(&dest_unchanged).expect("dest unchanged metadata");
    let ref_meta = fs::metadata(ref_dir.join("unchanged.txt")).expect("ref unchanged metadata");

    // Hard link verification: same inode and nlink > 1
    assert_eq!(
        dest_meta.ino(),
        ref_meta.ino(),
        "unchanged.txt in dest must share inode with reference (hard link)"
    );
    assert!(
        dest_meta.nlink() >= 2,
        "unchanged.txt nlink must be >= 2 (hard link), got {}",
        dest_meta.nlink()
    );

    // new_file.txt should be a regular copy (not in reference)
    let dest_new = dest_dir.join("new_file.txt");
    assert!(
        dest_new.exists(),
        "new_file.txt must exist at destination"
    );
    assert_eq!(
        fs::read(&dest_new).expect("read new_file.txt"),
        b"brand new\n",
        "new_file.txt content mismatch"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test for `--copy-dest` push over daemon protocol.
///
/// Verifies that a push transfer with `--copy-dest` copies unchanged files from
/// the reference directory into the destination instead of transferring them over
/// the wire, while changed or new files are still transferred normally.
///
/// # Scenario
///
/// Source (client side):
///   unchanged.txt  (content matches reference)
///   changed.txt    (content differs from reference)
///
/// Reference directory (on daemon, sibling of dest):
///   unchanged.txt  (same content as source)
///   changed.txt    (different content from source)
///
/// Destination (daemon module, initially empty):
///   After push, both files should appear. unchanged.txt should be a local copy
///   from the reference (same content, different inode). changed.txt should
///   contain the new source content.
///
/// # Upstream Reference
///
/// - `generator.c:recv_generator()` - copy_dest copies matching file locally
/// - `util2.c:copy_file()` - performs the local file copy
#[cfg(unix)]
#[test]
fn daemon_copy_dest_push_copies_from_reference() {
    use std::os::unix::fs::MetadataExt;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    fs::write(source_dir.join("unchanged.txt"), b"shared content\n").expect("write unchanged");
    fs::write(source_dir.join("changed.txt"), b"new version\n").expect("write changed");

    // --- Reference directory (sibling of dest) ---
    let ref_dir = temp.path().join("reference");
    fs::create_dir(&ref_dir).expect("create reference");

    fs::write(ref_dir.join("unchanged.txt"), b"shared content\n").expect("write ref unchanged");
    fs::write(ref_dir.join("changed.txt"), b"old version\n").expect("write ref changed");

    // Backdate reference files so quick-check uses size+mtime
    let old_time = filetime::FileTime::from_unix_time(1_000_000, 0);
    filetime::set_file_mtime(ref_dir.join("unchanged.txt"), old_time)
        .expect("backdate ref unchanged");
    filetime::set_file_mtime(ref_dir.join("changed.txt"), old_time)
        .expect("backdate ref changed");

    // Match source unchanged.txt mtime to reference
    filetime::set_file_mtime(source_dir.join("unchanged.txt"), old_time)
        .expect("backdate source unchanged");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config ---
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

    // --- Run client push with --copy-dest ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .copy_destination(&ref_dir)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(_summary) => {}
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("copy-dest client push failed: {e}");
        }
    }

    // unchanged.txt should exist as a local copy from reference (same content, different inode)
    let dest_unchanged = dest_dir.join("unchanged.txt");
    assert!(
        dest_unchanged.exists(),
        "unchanged.txt must exist at destination"
    );
    assert_eq!(
        fs::read(&dest_unchanged).expect("read unchanged.txt"),
        b"shared content\n",
        "unchanged.txt content must match source and reference"
    );

    let dest_meta = fs::metadata(&dest_unchanged).expect("dest unchanged metadata");
    let ref_meta = fs::metadata(ref_dir.join("unchanged.txt")).expect("ref unchanged metadata");

    // copy-dest creates a new file (different inode from reference, unlike link-dest)
    assert_ne!(
        dest_meta.ino(),
        ref_meta.ino(),
        "unchanged.txt in dest must have different inode from reference (copy, not hard link)"
    );

    // changed.txt should contain the new source content
    let dest_changed = dest_dir.join("changed.txt");
    assert!(
        dest_changed.exists(),
        "changed.txt must exist at destination"
    );
    assert_eq!(
        fs::read(&dest_changed).expect("read changed.txt"),
        b"new version\n",
        "changed.txt content must match source"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
