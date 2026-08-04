// QUIC listener port selection (oc extension). The `quic port` global directive
// overrides the QUIC bind port independently of the daemon TCP `port`. Unset,
// the QUIC listener shares the daemon `port` (873 by default). A `quic port = 0`
// coerces to the well-known rsync port 873, mirroring the TCP `port = 0` path.
// Decision on 2026-07-30: QUIC binds the SAME port as TCP by default.
//
// These tests read only the crate-visible accessors (`effective_quic_port`,
// `rsync_port`); the private `port` field and module-private `DEFAULT_PORT` are
// out of reach from this `tests` submodule, so the well-known port is spelled
// as the literal 873.

#[test]
fn quic_port_unset_shares_daemon_port() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "# no quic port directive\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    // With the directive unset the QUIC listener binds the default daemon port.
    assert_eq!(options.effective_quic_port(), 873);
}

#[test]
fn quic_port_unset_tracks_overridden_daemon_port() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "port = 9999\n[m]\npath = /srv/m\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    // A non-default daemon port with no `quic port` still shares it: the
    // effective QUIC port follows `quic_port.unwrap_or(port)`.
    assert_eq!(options.rsync_port(), Some(9999));
    assert_eq!(options.effective_quic_port(), 9999);
}

#[test]
fn quic_port_directive_overrides_daemon_port_independently() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "port = 8022\nquic port = 8873\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    // `quic port` is independent of the TCP `port`.
    assert_eq!(options.rsync_port(), Some(8022));
    assert_eq!(options.effective_quic_port(), 8873);
}

#[test]
fn quic_port_zero_coerces_to_well_known_rsync_port() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "port = 8022\nquic port = 0\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    // `quic port = 0` coerces to 873, matching the TCP `port = 0` / `--port 0`
    // coercion - not the daemon TCP port, and not a kernel-assigned ephemeral.
    assert_eq!(
        options.effective_quic_port(),
        873,
        "quic port = 0 must coerce to the well-known rsync port 873"
    );
}

#[test]
fn quic_port_in_module_scope_is_config_error() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    // The QUIC listener is shared across modules, so a per-module `quic port`
    // cannot be honoured and is a hard config error like the QUIC identity
    // directives, not silently ignored.
    fs::write(&config_path, "[data]\npath = /srv/data\nquic port = 8873\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect_err("module-scoped quic port is a config error");

    assert!(
        error.to_string().contains("quic port"),
        "error must name the offending directive, got: {error}"
    );
}
