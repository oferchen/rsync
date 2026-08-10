// DMC-6: verifies a per-module `max connections = N` directive coexists
// with the daemon-global `--max-connections` CLI flag and that the
// per-module value is what `ModuleRuntime` enforces for connections to
// that module, regardless of the higher global cap.
//
// upstream: target/interop/upstream-src/rsync-3.4.1/loadparm.c associates
// `max connections` with each `[modulename]` section (Locals: in
// `daemon-parm.txt:48`). Upstream has no daemon-global cap; oc-rsync's
// `--max-connections` is an additional daemon-level admission limit and
// must not override the per-module value when the module sets its own
// (a per-module value of 1 must still cap that module at 1 even when
// the global cap is 10).

#[test]
fn runtime_options_per_module_max_connections_overrides_global() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[capped]\npath = /srv/capped\nuse chroot = no\nmax connections = 1\n[uncapped]\npath = /srv/uncapped\nuse chroot = no\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--max-connections"),
        OsString::from("10"),
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("config parses");

    // Per-module cap on `[capped]` survives the parse and binds at 1,
    // independent of the global value of 10 set via `--max-connections`.
    // The global cap lives on `RuntimeOptions::max_connections` and is
    // covered separately by `parse_max_connections_option`; this test
    // pins the per-module field so a future refactor that merged the two
    // values would fail loudly.
    let capped = options
        .modules
        .iter()
        .find(|m| m.name == "capped")
        .expect("[capped] module present");
    assert_eq!(
        capped.max_connections(),
        NonZeroU32::new(1),
        "per-module 'max connections = 1' overrides the higher global cap",
    );

    // Sibling module without its own directive remains unlimited at the
    // module level (only the daemon-global cap applies to it).
    let uncapped = options
        .modules
        .iter()
        .find(|m| m.name == "uncapped")
        .expect("[uncapped] module present");
    assert!(
        uncapped.max_connections().is_none(),
        "modules without 'max connections' must not inherit the global cap as a per-module value",
    );
}
