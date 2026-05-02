/// End-to-end test for the `fake super = yes` daemon module directive.
///
/// Verifies that the daemon honours its module-config `fake super = yes`
/// directive end-to-end: a non-root client pushes a file with `--owner`
/// and `--group`, and the daemon receiver stores the ownership metadata
/// in the `user.rsync.%stat` xattr instead of calling `chown` (which
/// would fail without privileges).
///
/// # Wiring under test
///
/// `ModuleConfig.fake_super` -> `ServerConfig.fake_super` ->
/// `MetadataOptions.fake_super` -> `apply_ownership_via_fake_super`.
///
/// Without the wire-up, the directive is parsed but never reaches the
/// receiver, so a non-root daemon pushing with `-og` either fails the
/// chown silently or leaves no metadata at all.
///
/// # Filesystem probe
///
/// User-namespace xattrs (`user.*`) are required. Some filesystems
/// (e.g. tmpfs on older kernels) do not support them; the test probes
/// for support and skips gracefully when absent.
///
/// # Upstream Reference
///
/// - `clientserver.c:1106-1107` - daemon `fake super = yes` demotes the
///   receiver's `am_root` and forces fake-super semantics.
/// - `loadparm.c` - `fake super` module parameter.
/// - `rsync.c:set_file_attrs()` - fake-super stores ownership in xattrs.
/// - `xattrs.c:set_stat_xattr()` - encodes mode/uid/gid into the
///   `user.rsync.%stat` xattr.
#[cfg(unix)]
#[test]
fn daemon_fake_super_module_directive_stores_ownership_in_xattr() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // Probe the filesystem for user-namespace xattr support before going
    // through the full daemon dance. tmpfs on older kernels and some CI
    // overlays reject user.* writes outright.
    let probe_file = temp.path().join(".fake_super_probe");
    fs::write(&probe_file, b"probe").expect("write probe file");
    if xattr::set(&probe_file, "user.probe", b"1").is_err() {
        eprintln!(
            "skipping daemon_fake_super_module_directive_stores_ownership_in_xattr: \
             filesystem does not support user xattrs"
        );
        return;
    }
    fs::remove_file(&probe_file).expect("remove probe file");

    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    let payload_path = source_dir.join("ledger.txt");
    fs::write(&payload_path, b"fake-super module payload\n").expect("write source payload");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[fakesuper]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n\
         fake super = yes\n",
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
    let rsync_url = format!("rsync://127.0.0.1:{port}/fakesuper/");

    // -o + -g make the receiver's MetadataOptions request ownership
    // preservation. Without `fake super = yes` wired through, the daemon
    // would attempt chown(); with it, ownership is recorded in the
    // user.rsync.%stat xattr instead.
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .owner(true)
        .group(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 1,
                "expected at least 1 file transferred, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("fake-super module push failed: {e}");
        }
    }

    let dest_payload = dest_dir.join("ledger.txt");
    assert!(
        dest_payload.exists(),
        "ledger.txt must exist at the destination after the push"
    );
    assert_eq!(
        fs::read(&dest_payload).expect("read dest payload"),
        b"fake-super module payload\n",
        "destination payload content must match the source"
    );

    // The wire-up assertion: with `fake super = yes` on the module, the
    // receiver must have stored ownership in the user.rsync.%stat xattr.
    // upstream: xattrs.c:set_stat_xattr() encodes mode/uid/gid into this
    // single xattr, used by xattrs.c:read_stat_xattr() on restore.
    let stat_xattr = xattr::get(&dest_payload, "user.rsync.%stat")
        .expect("read user.rsync.%stat from destination payload")
        .expect(
            "user.rsync.%stat must be present on the destination when \
             `fake super = yes` is configured on the daemon module",
        );

    let stat_text = String::from_utf8(stat_xattr).expect("user.rsync.%stat must be UTF-8");
    // upstream: xattrs.c:set_stat_xattr() format is "<mode_octal> <rdev_major>,<rdev_minor> <uid>:<gid>".
    assert!(
        stat_text.contains(":"),
        "user.rsync.%stat must encode uid:gid (got {stat_text:?})"
    );
    assert!(
        stat_text.contains(","),
        "user.rsync.%stat must encode rdev_major,rdev_minor (got {stat_text:?})"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
