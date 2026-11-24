/// A module with `use chroot` unset munges symlinks exactly when the daemon's
/// chroot probe fails: a rootless daemon serves the module unchrooted, so the
/// `munge symlinks` default must come from the probed value, not the unset
/// one.
///
/// # Upstream Reference
///
/// - `clientserver.c:886-899` - an unset `use_chroot` probes `chroot("/")` and
///   switches to false on failure.
/// - `clientserver.c:1074-1075` - `munge_symlinks = !use_chroot ||
///   module_dirlen` reads the switched value.
#[cfg(unix)]
#[test]
fn daemon_munge_symlinks_default_follows_the_probed_use_chroot() {
    use std::os::unix::fs as unix_fs;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    unix_fs::symlink("/etc/passwd", source_dir.join("abs_link")).expect("create abs_link");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // A root daemon would drop to `nobody` (clientserver.c:837) and could
    // not write the root-owned tempdir; pin it to root so both privilege
    // levels run the same push. A non-root daemon never drops.
    let identity = if platform::privilege::is_effective_root() {
        "uid = 0\ngid = 0\n"
    } else {
        ""
    };
    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[mungemod]\npath = {}\nread only = false\n{identity}",
            dest_dir.display()
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
            OsString::from("2"),
        ])
        .build();
    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([
            source_arg,
            OsString::from(format!("rsync://127.0.0.1:{port}/mungemod/")),
        ])
        .links(true)
        .build();
    if let Err(e) = core::client::run_client(client_config) {
        drop(daemon_handle);
        panic!("daemon push failed: {e}");
    }

    let target = fs::read_link(dest_dir.join("abs_link")).expect("read abs_link target");
    if platform::privilege::probe_chroot_capability().is_ok() {
        // Chrooted: munging defaults off. Only the prefix is asserted; how a
        // chrooted session spells the unmunged target is a separate question.
        assert!(
            !target.starts_with("/rsyncd-munged"),
            "a chrooted module must not munge by default, got {target:?}",
        );
    } else {
        assert_eq!(
            target,
            Path::new("/rsyncd-munged//etc/passwd"),
            "an unset `use chroot` must take the probed value before the \
             `munge symlinks` default is derived from it",
        );
    }

    drop(daemon_handle);
}
