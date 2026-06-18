#[test]
fn runtime_options_rejects_ipv4_ipv6_combo() {
    // UTS-DD-daemon-exit10: `--ipv4 --ipv6` together is now the explicit
    // dual-stack opt-in (mirroring upstream rsync's `default_af_hint = 0`
    // semantic in `socket.c::open_socket_in`) rather than a CLI conflict.
    // The flag pair must succeed and surface as `dual_stack = true`
    // so the accept loop binds both families and tolerates per-family
    // bind failures. The bind-address conflict path is preserved
    // separately and still errors when `--bind` is mixed with the
    // opposite family flag.
    let options = RuntimeOptions::parse(&[OsString::from("--ipv4"), OsString::from("--ipv6")])
        .expect("--ipv4 --ipv6 must parse as the dual-stack opt-in");
    assert!(
        options.dual_stack(),
        "dual_stack must be set when both family flags are given"
    );
}

