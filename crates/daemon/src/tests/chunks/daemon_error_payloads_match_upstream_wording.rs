// Verifies the daemon's `@ERROR:` payload constants match upstream
// rsync 3.4.1 wire wording byte-for-byte.
//
// Backup tooling and log aggregators parse these specific strings, so any
// drift breaks downstream classification. Each row pins one upstream
// citation to its oc-rsync constant.
//
// upstream: clientserver.c:730,733,748,752,762

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
}
