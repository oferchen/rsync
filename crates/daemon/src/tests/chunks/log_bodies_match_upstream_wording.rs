/// The daemon's connection and auth log-line bodies must reproduce upstream
/// rsync's exact `rprintf(FLOG, ...)` format strings byte-for-byte (only the
/// timestamp/pid prefix, handled by the logging sink, differs).
///
/// upstream: clientserver.c:742 / :729 / :1421 / :1434 and authenticate.c:249.
#[test]
fn log_bodies_match_upstream_wording() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_log_sink(&path, Brand::Oc).expect("open log");

    let host = Some("client.example");
    let ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 17));
    let addr = std::net::SocketAddr::new(ip, 873);

    // upstream: clientserver.c:742 - module access granted.
    log_module_request(&log, host, ip, "docs");
    // upstream: authenticate.c:249/:335 - auth failure prefix.
    log_module_auth_failure(&log, host, ip, "docs");
    // upstream: clientserver.c:729 - access denied by allow/deny rules.
    log_module_denied(&log, host, ip, "docs");
    // upstream: clientserver.c:1434 - request for an undefined module.
    log_unknown_module(&log, host, ip, "docs");
    // upstream: clientserver.c:1421 - `#list` / empty module request.
    log_list_request(&log, host, addr);

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");

    for expected in [
        "rsync allowed access on module docs from client.example (192.0.2.17)",
        "auth failed on module docs from client.example (192.0.2.17)",
        "rsync denied on module docs from client.example (192.0.2.17)",
        "unknown module 'docs' tried from client.example (192.0.2.17)",
        "module-list request from client.example (192.0.2.17)",
    ] {
        assert!(
            contents.contains(expected),
            "missing upstream-exact log body {expected:?} in: {contents:?}"
        );
    }

    // Guard against regressing to the previous oc-invented phrasings.
    // (Note: "list request from" is intentionally omitted - it is a substring
    // of the correct "module-list request from" wording.)
    for forbidden in [
        "module 'docs' requested from",
        "authentication succeeded for module",
        "authentication failed for module",
        "access denied to module",
        "unknown module 'docs' requested from",
    ] {
        assert!(
            !contents.contains(forbidden),
            "oc-invented wording {forbidden:?} leaked into: {contents:?}"
        );
    }
}
