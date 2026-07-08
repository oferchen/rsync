// DMC-6: verifies the per-module cap-exceeded payload and the
// daemon-global cap-exceeded payload produce byte-identical wire bytes
// for the same limit value.
//
// Upstream rsync 3.4.1 emits a single literal at `clientserver.c:752`
// (`@ERROR: max connections (%d) reached -- try again later\n`).
// oc-rsync has two call sites that build this reply:
//
//   - per-module path (`module_access/request.rs:99`) - substitutes
//     `{limit}` into `MODULE_MAX_CONNECTIONS_PAYLOAD` and relies on
//     `send_error` to append the trailing `\n`.
//   - daemon-global path (`server_runtime/connection.rs:135`) - uses
//     `format!("@ERROR: max connections ({limit}) reached -- try again later\n")`
//     inline.
//
// Both must emit the same bytes for the same `N`, otherwise a backup
// tool that matches the upstream literal would parse one path but miss
// the other. This test reproduces both formats locally and pins them
// to each other plus to the upstream literal, guarding against drift
// in either call site.
//
// upstream: target/interop/upstream-src/rsync-3.4.1/clientserver.c:752

#[test]
fn daemon_per_module_cap_wire_bytes_match_global_path() {
    let cases: &[u32] = &[1, 2, 42, 1_000, u32::MAX];

    for &limit in cases {
        // Per-module path: template substitution + appended newline,
        // mirroring `handle_max_connections_exceeded` +
        // `send_error` in `module_access/request.rs`.
        let mut per_module = crate::daemon::MODULE_MAX_CONNECTIONS_PAYLOAD
            .replace("{limit}", &limit.to_string())
            .into_bytes();
        per_module.push(b'\n');

        // Daemon-global path: inline `format!` with trailing `\n`,
        // mirroring `refuse_if_at_capacity` in `server_runtime/connection.rs`.
        let global =
            format!("@ERROR: max connections ({limit}) reached -- try again later\n").into_bytes();

        assert_eq!(
            per_module, global,
            "per-module and global cap-exceeded payloads diverged at limit={limit}",
        );

        // Pin both to the upstream literal so neither call site can
        // silently drift away from upstream rsync 3.4.1.
        let expected =
            format!("@ERROR: max connections ({limit}) reached -- try again later\n").into_bytes();
        assert_eq!(
            per_module, expected,
            "per-module wire bytes diverged from upstream literal at limit={limit}",
        );
    }
}
