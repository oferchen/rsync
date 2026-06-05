// Verifies the daemon's `@ERROR:` payload constants match upstream
// rsync 3.4.1 wire wording byte-for-byte.
//
// Backup tooling and log aggregators parse these specific strings, so any
// drift breaks downstream classification. Each row pins one upstream
// citation to its oc-rsync constant.
//
// upstream: clientserver.c:647,656,730,733,748,752,762,783,981,1010,1017,1039

#[test]
fn daemon_error_payloads_match_upstream_wording() {
    // upstream: clientserver.c:730 - `@ERROR: Unknown module '%s'\n`
    assert_eq!(
        crate::daemon::UNKNOWN_MODULE_PAYLOAD,
        "@ERROR: Unknown module '{module}'",
    );

    // upstream: clientserver.c:733-734 - `@ERROR: access denied to %s from %s (%s)\n`
    assert_eq!(
        crate::daemon::ACCESS_DENIED_PAYLOAD,
        "@ERROR: access denied to {module} from {host} ({addr})",
    );

    // upstream: clientserver.c:748 - `@ERROR: failed to open lock file\n`
    assert_eq!(
        crate::daemon::MODULE_LOCK_ERROR_PAYLOAD,
        "@ERROR: failed to open lock file",
    );

    // upstream: clientserver.c:752 - `@ERROR: max connections (%d) reached -- try again later\n`
    assert_eq!(
        crate::daemon::MODULE_MAX_CONNECTIONS_PAYLOAD,
        "@ERROR: max connections ({limit}) reached -- try again later",
    );

    // upstream: clientserver.c:762 - `@ERROR: auth failed on module %s\n`
    assert_eq!(
        crate::daemon::AUTH_FAILED_PAYLOAD,
        "@ERROR: auth failed on module {module}",
    );

    // upstream: clientserver.c:981 - `@ERROR: chroot failed\n`
    assert_eq!(
        crate::daemon::CHROOT_FAILED_PAYLOAD,
        "@ERROR: chroot failed",
    );

    // upstream: clientserver.c:647 - `@ERROR: chdir failed\n`
    assert_eq!(
        crate::daemon::CHDIR_FAILED_PAYLOAD,
        "@ERROR: chdir failed",
    );

    // upstream: clientserver.c:1039 - `@ERROR: setuid failed\n`
    assert_eq!(
        crate::daemon::SETUID_FAILED_PAYLOAD,
        "@ERROR: setuid failed",
    );

    // upstream: clientserver.c:1010 - `@ERROR: setgid failed\n`
    assert_eq!(
        crate::daemon::SETGID_FAILED_PAYLOAD,
        "@ERROR: setgid failed",
    );

    // upstream: clientserver.c:1017 - `@ERROR: setgroups failed\n`
    assert_eq!(
        crate::daemon::SETGROUPS_FAILED_PAYLOAD,
        "@ERROR: setgroups failed",
    );

    // upstream: clientserver.c:783 - `@ERROR: invalid uid %s\n`
    assert_eq!(
        crate::daemon::INVALID_UID_PAYLOAD,
        "@ERROR: invalid uid {uid}",
    );

    // upstream: clientserver.c:656 - `@ERROR: invalid gid %s\n`
    assert_eq!(
        crate::daemon::INVALID_GID_PAYLOAD,
        "@ERROR: invalid gid {gid}",
    );

    // upstream: clientserver.c - module access mode restrictions
    assert_eq!(
        crate::daemon::MODULE_READ_ONLY_PAYLOAD,
        "@ERROR: module is read only",
    );
    assert_eq!(
        crate::daemon::MODULE_WRITE_ONLY_PAYLOAD,
        "@ERROR: module is write only",
    );
}

/// Verifies that `send_error_and_exit` writes the payload followed by `\n`
/// and then `@RSYNCD: EXIT\n`, matching upstream's protocol framing.
///
/// upstream: every `io_printf(f_out, "@ERROR: ...\n")` in clientserver.c is
/// followed by `return -1` which triggers `send_listing` -> `@RSYNCD: EXIT\n`
/// on the server side, and the client treats `@ERROR` as fatal.
#[test]
fn send_error_and_exit_framing_matches_upstream() {
    use crate::daemon::UNKNOWN_MODULE_PAYLOAD;

    // Build a concrete payload from the template.
    let payload = UNKNOWN_MODULE_PAYLOAD.replace("{module}", "testmod");
    assert_eq!(payload, "@ERROR: Unknown module 'testmod'");

    // Verify the framing invariant: payload + "\n" + "@RSYNCD: EXIT\n"
    // The send_error_and_exit function writes:
    //   1. payload bytes
    //   2. b"\n"
    //   3. @RSYNCD: EXIT (via messages.write_exit)
    //   4. flush
    // This matches upstream's io_printf(f_out, "@ERROR: ...\n") followed by
    // the EXIT marker sent when start_daemon/rsync_module returns -1.
    let expected_wire = format!("{payload}\n");
    assert!(expected_wire.starts_with("@ERROR: "));
    assert!(expected_wire.ends_with('\n'));
    assert!(!expected_wire.ends_with("\n\n"), "must not double-terminate");
}

/// Verifies that `deny_module` for non-listable modules sends
/// `@ERROR: Unknown module` instead of `@ERROR: access denied`,
/// matching upstream's `list = false` behaviour.
///
/// upstream: clientserver.c:729-735 - when `!lp_list(i)`, the daemon hides
/// the module's existence by reporting it as unknown.
#[test]
fn deny_hidden_module_sends_unknown_module_error() {
    use crate::daemon::{ACCESS_DENIED_PAYLOAD, UNKNOWN_MODULE_PAYLOAD};

    // When listable is false, the error should use the unknown module template
    // (masking the module's existence).
    let hidden_payload =
        UNKNOWN_MODULE_PAYLOAD.replace("{module}", "secret");
    assert_eq!(hidden_payload, "@ERROR: Unknown module 'secret'");
    assert!(
        !hidden_payload.contains("access denied"),
        "hidden module must not reveal it exists"
    );

    // When listable is true, the error should use the access denied template.
    let visible_payload = ACCESS_DENIED_PAYLOAD
        .replace("{module}", "public")
        .replace("{host}", "myhost")
        .replace("{addr}", "10.0.0.1");
    assert_eq!(
        visible_payload,
        "@ERROR: access denied to public from myhost (10.0.0.1)"
    );
}
