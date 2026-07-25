/// A `refuse options` directive repeated in the global section replaces the
/// earlier list instead of accumulating both.
///
/// upstream: loadparm.c:452-454 writes the `refuse_options` P_STRING slot with
/// string_set(), so the daemon refuses only what the last directive named.
#[test]
fn runtime_options_last_wins_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "refuse options = compress\nrefuse options = delete\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("repeated global refuse options is accepted");

    assert_eq!(options.modules()[0].refused_options(), ["delete"]);
}
