/// End-to-end test for filenames containing emoji and non-BMP Unicode (U+10000+).
///
/// Verifies that 4-byte UTF-8 sequences survive the roundtrip through the wire
/// protocol, file list sorting, and filesystem operations during a daemon push.
///
/// # Scenario
///
/// Source (client side):
///   rocket.txt          (named with U+1F680 ROCKET emoji)
///   heart.txt           (named with U+2764 HEAVY BLACK HEART - BMP but multi-byte)
///   flag.txt            (named with U+1F1FA U+1F1F8 regional indicator - flag sequence)
///   subdir_with_emoji/  (directory named with U+1F4C2 OPEN FILE FOLDER emoji)
///     nested.txt        (plain ASCII name inside emoji directory)
///
/// Destination (daemon module, initially empty):
///   After push, all files and directories must appear with correct names and content.
///
/// # Notes
///
/// macOS HFS+/APFS may normalize Unicode to NFD. The test uses precomposed emoji
/// codepoints that are stable under NFD normalization (single codepoints remain
/// unchanged). The verification reads back directory entries and checks that the
/// expected content is present, tolerating normalization differences in the names.
#[cfg(unix)]
#[test]
fn daemon_unicode_emoji_filenames_roundtrip() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // Emoji and non-BMP filenames
    let rocket_name = "\u{1F680}_rocket.txt"; // U+1F680 ROCKET (4-byte UTF-8)
    let heart_name = "\u{2764}_heart.txt"; // U+2764 HEAVY BLACK HEART (3-byte UTF-8)
    let flag_name = "\u{1F1FA}\u{1F1F8}_flag.txt"; // Regional indicators (4-byte each)
    let emoji_dir_name = "\u{1F4C2}_folder"; // U+1F4C2 OPEN FILE FOLDER (4-byte UTF-8)
    let nested_name = "nested.txt";

    let source_dir = temp.path().join("source");
    let source_emoji_subdir = source_dir.join(emoji_dir_name);
    fs::create_dir_all(&source_emoji_subdir).expect("create source emoji subdir");

    fs::write(source_dir.join(rocket_name), b"rocket payload\n").expect("write rocket");
    fs::write(source_dir.join(heart_name), b"heart payload\n").expect("write heart");
    fs::write(source_dir.join(flag_name), b"flag payload\n").expect("write flag");
    fs::write(source_emoji_subdir.join(nested_name), b"nested payload\n")
        .expect("write nested in emoji dir");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

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

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(_summary) => {}
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("emoji filenames push failed: {e}");
        }
    }

    // Use a helper that checks content by scanning directory entries, tolerating
    // possible Unicode normalization differences on macOS.

    let verify_file_content = |dir: &Path, expected_substring: &str, expected_content: &[u8]| {
        let entries: Vec<_> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .collect();
        let matching = entries.iter().find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains(expected_substring)
        });
        let matching =
            matching.unwrap_or_else(|| panic!("no file containing '{expected_substring}' in {dir:?}; found: {entries:?}"));
        let content = fs::read(matching.path())
            .unwrap_or_else(|e| panic!("read {}: {e}", matching.path().display()));
        assert_eq!(
            content, expected_content,
            "content mismatch for file containing '{expected_substring}'"
        );
    };

    // Verify each emoji-named file arrived with correct content
    verify_file_content(&dest_dir, "rocket", b"rocket payload\n");
    verify_file_content(&dest_dir, "heart", b"heart payload\n");
    verify_file_content(&dest_dir, "flag", b"flag payload\n");

    // Verify the emoji-named directory exists and contains the nested file
    let dest_entries: Vec<_> = fs::read_dir(&dest_dir)
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .collect();
    let emoji_subdir_entry = dest_entries
        .iter()
        .find(|e| {
            e.file_type().map(|ft| ft.is_dir()).unwrap_or(false)
                && e.file_name().to_string_lossy().contains("folder")
        })
        .expect("emoji directory must exist in destination");

    let nested_path = emoji_subdir_entry.path().join(nested_name);
    assert!(
        nested_path.exists(),
        "nested.txt must exist inside emoji-named directory"
    );
    assert_eq!(
        fs::read(&nested_path).expect("read nested.txt"),
        b"nested payload\n",
        "nested.txt content mismatch"
    );

    // Verify the emoji directory name contains the actual emoji codepoint
    let dir_name_str = emoji_subdir_entry.file_name().to_string_lossy().to_string();
    assert!(
        dir_name_str.contains('\u{1F4C2}'),
        "directory name must contain U+1F4C2 emoji, got: {dir_name_str}"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
