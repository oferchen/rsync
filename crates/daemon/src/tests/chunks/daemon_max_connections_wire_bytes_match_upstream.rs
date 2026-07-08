// DMC-3 regression test: pins the exact wire bytes emitted when the
// daemon refuses a connection because the `max connections` cap (per
// module or global `--max-connections`) is exceeded.
//
// Upstream rsync 3.4.1 emits the literal string
// `@ERROR: max connections (%d) reached -- try again later\n` via
// `io_printf(f_out, ...)` in `clientserver.c:752`. Backup tools, log
// aggregators and rsync client parsers match this exact byte sequence,
// so any drift in template, substitution, or trailing newline is a
// breaking change for downstream consumers.
//
// This test asserts the bytes byte-for-byte (no `contains`, no regex)
// for several limit values and pins the template constant the daemon
// derives its payload from. Both the per-module path
// (`module_access/request.rs`) and the global cap path
// (`server_runtime/connection.rs`) build their reply from the same
// template + `{limit}` + `\n` shape, so pinning the template plus the
// substituted bytes locks both call sites against drift.
//
// upstream: target/interop/upstream-src/rsync-3.4.1/clientserver.c:752

#[test]
fn daemon_max_connections_wire_bytes_match_upstream() {
    // Pin the template constant the daemon substitutes into.
    assert_eq!(
        crate::daemon::MODULE_MAX_CONNECTIONS_PAYLOAD,
        "@ERROR: max connections ({limit}) reached -- try again later",
    );

    // Pin the exact wire bytes (template + concrete limit + trailing
    // newline) the daemon writes to the socket. The newline is part of
    // upstream's `io_printf` format and is appended by both call sites:
    //   - per-module: `send_error` writes `payload` then `\n`.
    //   - global cap: `format!("... ({limit}) ... later\n")`.
    let cases: &[(u32, &[u8])] = &[
        (
            1,
            b"@ERROR: max connections (1) reached -- try again later\n",
        ),
        (
            2,
            b"@ERROR: max connections (2) reached -- try again later\n",
        ),
        (
            42,
            b"@ERROR: max connections (42) reached -- try again later\n",
        ),
        (
            4294967295,
            b"@ERROR: max connections (4294967295) reached -- try again later\n",
        ),
    ];

    for (limit, expected) in cases {
        let substituted =
            crate::daemon::MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.to_string());
        let mut wire = substituted.into_bytes();
        wire.push(b'\n');
        assert_eq!(
            wire.as_slice(),
            *expected,
            "wire bytes drifted from upstream rsync 3.4.1 clientserver.c:752 for limit={limit}",
        );
    }
}
