/// End-to-end tests for daemon module filter rules merging with client `--filter`.
///
/// Verifies the precedence rule that daemon-side filters always run BEFORE
/// client-side filters: a server-side `exclude = ...` cannot be overridden by
/// a client-side `+ ...` rule, and an `include = ...` followed by `exclude = *`
/// keeps only the explicitly-included paths reachable.
///
/// # Upstream Reference
///
/// - `clientserver.c:874-893` - daemon parses `filter`, `include from`, `include`,
///   `exclude from`, `exclude` directives and builds `daemon_filter_list`
/// - `exclude.c:1010-1025` - the receiver consults `daemon_filter_list` BEFORE
///   the user's `filter_list`, with first-match-wins semantics
/// - oc-rsync mirror: `crates/transfer/src/generator/filters.rs:66-75` (daemon
///   rules prepended to client wire rules)
#[cfg(unix)]
#[test]
fn daemon_exclude_overrides_client_include() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::write(source_dir.join("keep.txt"), b"plain text\n").expect("write keep.txt");
    fs::write(source_dir.join("error.log"), b"log content\n").expect("write error.log");
    fs::write(source_dir.join("debug.log"), b"debug content\n").expect("write debug.log");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[mod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n\
         exclude = *.log\n",
        source_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = crate::DaemonConfig::builder()
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

    // Client tries to re-include *.log via --filter='+ *.log' and --filter='+ /'
    // (server filter list is consulted first, so this MUST NOT win).
    let client_config = core::client::ClientConfig::builder()
        .recursive(true)
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_dir.as_os_str())])
        .add_filter_rule(core::client::FilterRuleSpec::include("*.log"))
        .build();

    let result = core::client::run_client(client_config);

    if let Err(e) = &result {
        let _ = daemon_handle.join();
        panic!("transfer failed: {e}");
    }

    assert!(
        dest_dir.join("keep.txt").exists(),
        "non-excluded file must transfer"
    );
    assert!(
        !dest_dir.join("error.log").exists(),
        "daemon `exclude = *.log` must override client --filter='+ *.log'"
    );
    assert!(
        !dest_dir.join("debug.log").exists(),
        "daemon `exclude = *.log` must override client --filter='+ *.log' (second file)"
    );

    let _ = daemon_handle.join().expect("daemon thread");
}

/// Daemon `exclude = secret/` blocks the entire subtree from being reachable.
///
/// Mirrors upstream `exclude.c:check_filter()` returning DEL_HIDE for the
/// subtree, which causes the daemon to skip the directory entirely during
/// the file list build.
#[cfg(unix)]
#[test]
fn daemon_exclude_directory_blocks_subtree() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir_all(source_dir.join("public")).expect("create public");
    fs::create_dir_all(source_dir.join("secret")).expect("create secret");
    fs::write(source_dir.join("public/readme.txt"), b"public\n").expect("write readme");
    fs::write(source_dir.join("secret/data.txt"), b"secret\n").expect("write secret data");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[mod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n\
         exclude = secret/\n",
        source_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = crate::DaemonConfig::builder()
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
        .recursive(true)
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_dir.as_os_str())])
        .build();

    let result = core::client::run_client(client_config);

    if let Err(e) = &result {
        let _ = daemon_handle.join();
        panic!("transfer failed: {e}");
    }

    assert!(
        dest_dir.join("public/readme.txt").exists(),
        "non-excluded subtree must transfer"
    );
    assert!(
        !dest_dir.join("secret").exists(),
        "daemon-excluded directory must not be transferred (entire subtree hidden)"
    );

    let _ = daemon_handle.join().expect("daemon thread");
}

/// Daemon `include = important/` followed by `exclude = *` keeps only the
/// explicitly-included subtree reachable.
///
/// Verifies the daemon's include-then-exclude pattern: the leading include
/// wins for matching paths, then the trailing `exclude = *` strips everything
/// else.
#[cfg(unix)]
#[test]
fn daemon_include_then_exclude_all_keeps_only_included() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir_all(source_dir.join("important")).expect("create important");
    fs::create_dir_all(source_dir.join("noise")).expect("create noise");
    fs::write(source_dir.join("important/data.txt"), b"keep\n").expect("write important");
    fs::write(source_dir.join("noise/junk.txt"), b"junk\n").expect("write noise");
    fs::write(source_dir.join("toplevel.txt"), b"top\n").expect("write top");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // Daemon module says: include important/ subtree, exclude everything else.
    // The trailing `*` strips noise/ and toplevel.txt; important/ survives
    // because the include rule was matched first.
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[mod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n\
         include = important/\n\
         include = important/**\n\
         exclude = *\n",
        source_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = crate::DaemonConfig::builder()
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
        .recursive(true)
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_dir.as_os_str())])
        .build();

    let result = core::client::run_client(client_config);

    if let Err(e) = &result {
        let _ = daemon_handle.join();
        panic!("transfer failed: {e}");
    }

    assert!(
        dest_dir.join("important/data.txt").exists(),
        "explicitly-included subtree must transfer"
    );
    assert!(
        !dest_dir.join("noise").exists(),
        "non-included sibling subtree must be excluded by trailing `exclude = *`"
    );
    assert!(
        !dest_dir.join("toplevel.txt").exists(),
        "non-included sibling file must be excluded by trailing `exclude = *`"
    );

    let _ = daemon_handle.join().expect("daemon thread");
}

/// Daemon `exclude from = <file>` produces the same effect as inline
/// `exclude = ...` rules.
///
/// This ensures the file-based form parses correctly and merges into
/// `daemon_filter_list` identically to the inline form.
#[cfg(unix)]
#[test]
fn daemon_exclude_from_file_matches_inline_exclude() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::write(source_dir.join("keep.dat"), b"data\n").expect("write keep.dat");
    fs::write(source_dir.join("skip.tmp"), b"temp\n").expect("write skip.tmp");
    fs::write(source_dir.join("skip.bak"), b"backup\n").expect("write skip.bak");

    let exclude_file = temp.path().join("excludes.txt");
    fs::write(&exclude_file, "*.tmp\n*.bak\n").expect("write excludes file");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[mod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n\
         exclude from = {}\n",
        source_dir.display(),
        exclude_file.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = crate::DaemonConfig::builder()
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

    // Client tries to re-include via --filter='+ *.tmp'; daemon must still
    // win and exclude both *.tmp and *.bak.
    let client_config = core::client::ClientConfig::builder()
        .recursive(true)
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_dir.as_os_str())])
        .add_filter_rule(core::client::FilterRuleSpec::include("*.tmp"))
        .build();

    let result = core::client::run_client(client_config);

    if let Err(e) = &result {
        let _ = daemon_handle.join();
        panic!("transfer failed: {e}");
    }

    assert!(
        dest_dir.join("keep.dat").exists(),
        "non-excluded file must transfer"
    );
    assert!(
        !dest_dir.join("skip.tmp").exists(),
        "exclude-from rule must override client include"
    );
    assert!(
        !dest_dir.join("skip.bak").exists(),
        "exclude-from rule must apply to all listed patterns"
    );

    let _ = daemon_handle.join().expect("daemon thread");
}
