/// An unreadable `motd file` leaves the daemon startable with no motd content.
///
/// upstream opens the motd per CONNECTION and keeps every failure local to the
/// greeting (clientserver.c:188-197): `open_no_attacker_symlinks` returning
/// `< 0` yields a NULL `FILE *`, the read loop is skipped, and the connection
/// proceeds. Config parsing has no motd open at all, so an unopenable motd
/// cannot stop the daemon from reaching `listen()`.
///
/// Propagating the read error here instead killed the daemon during config
/// parse - what the 3.5.0 `daemon-config-symlink` cell observes as `rsyncd
/// exited before listening`.
#[test]
fn runtime_options_unreadable_motd_is_not_fatal() {
    let dir = tempdir().expect("motd dir");
    let config_path = dir.path().join("rsyncd.conf");

    // A directory cannot be read as a file, and unlike a refused symlink it
    // fails identically on every platform and needs no second uid.
    fs::create_dir(dir.path().join("motd.txt")).expect("motd as a directory");

    fs::write(
        &config_path,
        "motd file = motd.txt\n[docs]\npath = /srv/docs\nuse chroot = no\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("an unreadable motd must not fail the parse");

    assert!(
        options.motd_lines().is_empty(),
        "an unreadable motd contributes no lines"
    );
    assert_eq!(options.modules().len(), 1, "the module is still configured");
}

/// The non-vacuity companion: the same config with a READABLE motd delivers
/// its line, so the assertion above is measuring the failure path rather than
/// a parser that never reads a motd at all.
#[test]
fn runtime_options_readable_motd_still_delivers_its_lines() {
    let dir = tempdir().expect("motd dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(dir.path().join("motd.txt"), "Greetings\n").expect("write motd");

    fs::write(
        &config_path,
        "motd file = motd.txt\n[docs]\npath = /srv/docs\nuse chroot = no\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with a readable motd");

    assert_eq!(options.motd_lines(), [String::from("Greetings")]);
}
