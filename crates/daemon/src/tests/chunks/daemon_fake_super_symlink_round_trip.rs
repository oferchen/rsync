/// A `fake super = yes` module must store a pushed symlink as a regular
/// placeholder holding the link target, and a pull from that module must
/// restore the real symlink.
///
/// Linux user xattrs cannot be set on a symlink, so upstream builds with
/// `NO_SYMLINK_USER_XATTRS` and the fake-super receiver writes each link as
/// a 0600 regular file whose content is the target. Its `%stat` records the
/// link type. The fake-super sender reads the target back from the file.
/// A receiver that creates a real symlink cannot record the link's
/// ownership. A sender that sends an empty target loses the link on the
/// pull.
///
/// # Upstream Reference
///
/// - `configure.ac:1466` - `NO_SYMLINK_USER_XATTRS` on Linux.
/// - `syscall.c:do_symlink_atfd()` - the `am_root < 0` placeholder create.
/// - `syscall.c:do_readlink()` - the `am_root < 0` placeholder read.
#[cfg(target_os = "linux")]
#[test]
fn daemon_fake_super_module_round_trips_symlinks_through_placeholders() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let temp = tempdir().expect("tempdir");
    let probe_file = temp.path().join(".fake_super_probe");
    fs::write(&probe_file, b"probe").expect("write probe file");
    if xattr::set(&probe_file, "user.probe", b"1").is_err() {
        eprintln!(
            "skipping daemon_fake_super_module_round_trips_symlinks_through_placeholders: \
             filesystem does not support user xattrs"
        );
        return;
    }
    fs::remove_file(&probe_file).expect("remove probe file");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");
    fs::write(source_dir.join("f"), b"payload\n").expect("write source file");
    std::os::unix::fs::symlink("f", source_dir.join("link")).expect("create link");
    std::os::unix::fs::symlink("/nonexistent/target", source_dir.join("dangle"))
        .expect("create dangling link");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");
    let back_dir = temp.path().join("back");
    fs::create_dir(&back_dir).expect("create pull dest");
    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[fakesuper]\n\
             path = {}\n\
             read only = false\n\
             use chroot = false\n\
             fake super = yes\n",
            module_dir.display()
        ),
    )
    .expect("write daemon config");
    let (port, held_listener) = allocate_test_port();
    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("3"),
        ])
        .build();
    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);
    let rsync_url = format!("rsync://127.0.0.1:{port}/fakesuper/");
    let run = |src: OsString, dst: OsString| {
        core::client::run_client(
            core::client::ClientConfig::builder()
                .transfer_args([src, dst])
                .recursive(true)
                .links(true)
                .owner(true)
                .group(true)
                .permissions(true)
                .build(),
        )
    };
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    if let Err(e) = run(source_arg, OsString::from(&rsync_url)) {
        let _ = finish_daemon(daemon_handle);
        panic!("push into the fake-super module failed: {e}");
    }
    let owner = fs::metadata(source_dir.join("f")).expect("stat source");
    let want_stat = metadata::FakeSuperStat {
        mode: 0o120777,
        uid: owner.uid(),
        gid: owner.gid(),
        rdev: None,
    };
    for (name, target) in [("link", "f"), ("dangle", "/nonexistent/target")] {
        let placeholder = module_dir.join(name);
        let meta = fs::symlink_metadata(&placeholder).expect("stat placeholder");
        assert!(
            meta.file_type().is_file(),
            "{name}: the module must hold a regular placeholder, not a {:?}",
            meta.file_type()
        );
        // A module without chroot munges links (clientserver.c:1070-1071), so the
        // placeholder holds the munged target and the pull strips it again.
        assert_eq!(
            fs::read(&placeholder).expect("read placeholder"),
            format!("/rsyncd-munged/{target}").as_bytes(),
            "{name}: the placeholder must hold the munged link target"
        );
        assert_eq!(
            meta.permissions().mode() & 0o7777,
            0o777,
            "{name}: the real mode is the link's 0777 with owner rw forced on"
        );
        assert_eq!(
            metadata::load_fake_super(&placeholder).expect("read %stat"),
            Some(want_stat.clone()),
            "{name}: %stat must record the symlink type and its owner"
        );
    }
    let mut back_arg = back_dir.clone().into_os_string();
    back_arg.push("/");
    if let Err(e) = run(OsString::from(&rsync_url), back_arg) {
        let _ = finish_daemon(daemon_handle);
        panic!("pull from the fake-super module failed: {e}");
    }
    let _ = finish_daemon(daemon_handle);
    for (name, target) in [("link", "f"), ("dangle", "/nonexistent/target")] {
        assert_eq!(
            fs::read_link(back_dir.join(name))
                .unwrap_or_else(|e| panic!("{name}: the pull must restore a symlink: {e}")),
            Path::new(target),
            "{name}: the pulled symlink must keep its target"
        );
    }
}
